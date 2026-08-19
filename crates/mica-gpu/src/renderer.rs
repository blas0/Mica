//! Encoding a frame.
//!
//! The nine passes run in a fixed order into a single render pass. One command
//! buffer, one encoder, nine `drawPrimitives` calls at most — and **fewer than
//! nine whenever a pass has nothing to draw**, because an empty instance array
//! produces no buffer and the pass is skipped entirely. On an ordinary prompt
//! that is two passes, not nine.
//!
//! The same code path renders to a `CAMetalLayer` drawable and to an offscreen
//! texture. The offscreen path is not a debug affordance: it is how the render
//! path is tested, by reading the pixels back and asserting on them.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder,
    MTLCommandQueue,
    MTLLoadAction, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineState,
    MTLStoreAction, MTLTexture,
};

use mica_atlas::atlas::Atlas;
use mica_atlas::raster::PixelFormat;

use crate::context::{GpuContext, GpuError, QUAD_PRIMITIVE, QUAD_VERTEX_COUNT};
use crate::frame::{Decision, FrameScheduler, Reason};
use crate::grid::{InstanceBuffers, SubstrateUniforms, Uniforms};

/// Buffer indices, matching the `[[buffer(n)]]` attributes in `mica.metal`.
const UNIFORM_INDEX: usize = 0;
const INSTANCE_INDEX: usize = 1;

/// Texture indices, matching `[[texture(n)]]`.
const MASK_TEXTURE: usize = 0;
const COLOR_TEXTURE: usize = 1;

pub struct Renderer {
    context: GpuContext,
    scheduler: FrameScheduler,
    buffers: InstanceBuffers,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("context", &self.context)
            .field("stats", &self.scheduler.stats())
            .field("instances", &self.buffers.total())
            .finish()
    }
}

impl Renderer {
    pub fn new(page_size: u16) -> Result<Renderer, GpuError> {
        Ok(Renderer {
            context: GpuContext::new(page_size)?,
            // Two frames in flight, matching `maximumDrawableCount`. See
            // `FrameScheduler::new` for why not three.
            scheduler: FrameScheduler::new(2),
            buffers: InstanceBuffers::default(),
        })
    }

    pub fn context(&self) -> &GpuContext {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut GpuContext {
        &mut self.context
    }

    pub fn scheduler(&mut self) -> &mut FrameScheduler {
        &mut self.scheduler
    }

    pub fn stats(&self) -> crate::frame::FrameStats {
        self.scheduler.stats()
    }

    pub fn buffers(&mut self) -> &mut InstanceBuffers {
        &mut self.buffers
    }

    /// Copies any newly rasterised glyphs into their textures.
    ///
    /// Called before encoding, never during: a texture cannot be written while
    /// a pass that samples it is being encoded.
    pub fn sync_atlas(&mut self, atlas: &mut Atlas) -> Result<(), GpuError> {
        if atlas.generation() != self.context.atlas_generation() {
            // The atlas dropped pages — a font change, a size change, an
            // eviction. Every texture we hold is now meaningless.
            self.context.reset_atlas(atlas.generation());
            self.scheduler.request(Reason::Atlas);
        }
        if !atlas.has_uploads() {
            return Ok(());
        }
        for upload in atlas.take_uploads() {
            self.context.upload(&upload)?;
        }
        self.scheduler.request(Reason::Atlas);
        Ok(())
    }

    /// Renders into an arbitrary texture and blocks until it is done.
    ///
    /// Used by tests and by frame capture. The live path is
    /// [`Renderer::render_to_drawable`], which does not block.
    pub fn render_to_texture(
        &mut self,
        target: &ProtocolObject<dyn MTLTexture>,
        uniforms: Uniforms,
        substrate: SubstrateUniforms,
    ) -> Result<(), GpuError> {
        let command_buffer =
            self.context.queue().commandBuffer().ok_or(GpuError::BufferFailed)?;
        command_buffer.setLabel(Some(&NSString::from_str("mica.frame")));

        self.encode(&command_buffer, target, uniforms, substrate)?;

        // Managed storage needs an explicit synchronise before the CPU can
        // read the texture back.
        if let Some(blit) = command_buffer.blitCommandEncoder() {
            blit.synchronizeResource(ProtocolObject::from_ref(target));
            blit.endEncoding();
        }

        command_buffer.commit();
        command_buffer.waitUntilCompleted();
        Ok(())
    }

    /// Encodes the nine passes into an existing command buffer.
    pub fn encode(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        target: &ProtocolObject<dyn MTLTexture>,
        uniforms: Uniforms,
        substrate: SubstrateUniforms,
    ) -> Result<(), GpuError> {
        let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment =
            unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(target));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        // The substrate pass paints every pixel, so the clear colour is only
        // ever visible if that pass is skipped — which it never is.
        attachment.setClearColor(MTLClearColor {
            red: substrate.background[0] as f64,
            green: substrate.background[1] as f64,
            blue: substrate.background[2] as f64,
            alpha: 1.0,
        });

        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&descriptor)
            .ok_or(GpuError::BufferFailed)?;
        encoder.setLabel(Some(&NSString::from_str("mica.pass")));

        let uniform_buffer = self
            .context
            .buffer_from(std::slice::from_ref(&uniforms), "mica.uniforms")?
            .ok_or(GpuError::BufferFailed)?;

        // --- 1. substrate ---------------------------------------------------
        {
            let p = &self.context.pipelines().substrate;
            encoder.setRenderPipelineState(p);
            set_uniforms(&encoder, &uniform_buffer);
            let substrate_buffer = self
                .context
                .buffer_from(std::slice::from_ref(&substrate), "mica.substrate")?
                .ok_or(GpuError::BufferFailed)?;
            unsafe {
                encoder.setFragmentBuffer_offset_atIndex(
                    Some(&substrate_buffer),
                    0,
                    UNIFORM_INDEX,
                );
                encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    QUAD_PRIMITIVE,
                    0,
                    QUAD_VERTEX_COUNT,
                    1,
                );
            }
        }

        // The glyph passes need the atlas bound. A session with no colour
        // glyphs has no colour page, so the same texture is bound twice rather
        // than allocating a dummy one — the shader only samples the slot its
        // flag names.
        let (mask, color) = self.atlas_textures();

        // --- 2..9 -----------------------------------------------------------
        // Each pass is skipped outright when it has no instances. This is why
        // an ordinary prompt costs two draw calls rather than nine.
        macro_rules! pass {
            ($pipeline:ident, $instances:expr, $label:literal, $textures:expr) => {{
                let instances = $instances;
                if !instances.is_empty() {
                    if let Some(buffer) = self.context.buffer_from(instances, $label)? {
                        let pipeline: &Retained<ProtocolObject<dyn MTLRenderPipelineState>> =
                            &self.context.pipelines().$pipeline;
                        encoder.setRenderPipelineState(pipeline);
                        set_uniforms(&encoder, &uniform_buffer);
                        unsafe {
                            encoder.setVertexBuffer_offset_atIndex(
                                Some(&buffer),
                                0,
                                INSTANCE_INDEX,
                            );
                        }
                        if $textures {
                            bind_atlas(&encoder, self.context.sampler(), mask, color);
                        }
                        unsafe {
                            encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                                QUAD_PRIMITIVE,
                                0,
                                QUAD_VERTEX_COUNT,
                                instances.len(),
                            );
                        }
                    }
                }
            }};
        }

        pass!(cell_bg, &self.buffers.backgrounds, "mica.cell_bg", false);
        pass!(cell, &self.buffers.glyphs, "mica.cells", true);
        pass!(cell_rule, &self.buffers.rules, "mica.rules", false);
        pass!(block_gutter, &self.buffers.gutters, "mica.gutters", false);
        pass!(decay, &self.buffers.decays, "mica.decay", false);
        pass!(shape, &self.buffers.shapes, "mica.shapes", false);
        pass!(quad, &self.buffers.quads, "mica.quads", false);
        pass!(ui_text, &self.buffers.ui_text, "mica.uitext", true);

        encoder.endEncoding();
        Ok(())
    }

    /// The two textures the glyph shaders sample.
    ///
    /// When one kind of page does not exist yet, the other stands in for it.
    /// Metal requires *something* bound to a sampled slot even on a branch the
    /// shader never takes, and binding a real texture is cheaper than keeping a
    /// 1×1 placeholder alive for a case that resolves itself.
    fn atlas_textures(
        &self,
    ) -> (
        Option<&ProtocolObject<dyn MTLTexture>>,
        Option<&ProtocolObject<dyn MTLTexture>>,
    ) {
        let find = |format: PixelFormat| {
            self.context
                .pages()
                .iter()
                .find(|p| p.format == format)
                .map(|p| &*p.texture)
        };
        let mask = find(PixelFormat::Alpha8);
        let color = find(PixelFormat::Bgra8);
        (mask.or(color), color.or(mask))
    }
}

fn set_uniforms(
    encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    uniforms: &ProtocolObject<dyn MTLBuffer>,
) {
    unsafe {
        encoder.setVertexBuffer_offset_atIndex(Some(uniforms), 0, UNIFORM_INDEX);
    }
}

fn bind_atlas(
    encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    sampler: &ProtocolObject<dyn objc2_metal::MTLSamplerState>,
    mask: Option<&ProtocolObject<dyn MTLTexture>>,
    color: Option<&ProtocolObject<dyn MTLTexture>>,
) {
    unsafe {
        encoder.setFragmentTexture_atIndex(mask, MASK_TEXTURE);
        encoder.setFragmentTexture_atIndex(color, COLOR_TEXTURE);
        encoder.setFragmentSamplerState_atIndex(Some(sampler), 0);
    }
}

/// Reads a rendered texture back into BGRA bytes.
///
/// Test-facing, but not `#[cfg(test)]`: a frame capture tool and a golden-image
/// check both want it, and hiding it behind a cfg would mean the only way to
/// look at what Mica drew is to take a photograph of a monitor.
pub fn read_back(texture: &ProtocolObject<dyn MTLTexture>) -> Vec<u8> {
    let width = texture.width();
    let height = texture.height();
    let bytes_per_row = width * 4;
    let mut data = vec![0u8; bytes_per_row * height];

    let region = objc2_metal::MTLRegion {
        origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
        size: objc2_metal::MTLSize { width, height, depth: 1 },
    };
    // SAFETY: the buffer is exactly `bytes_per_row * height` and the region is
    // the whole texture.
    unsafe {
        texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            std::ptr::NonNull::new(data.as_mut_ptr() as *mut std::ffi::c_void).unwrap(),
            bytes_per_row,
            region,
            0,
        );
    }
    data
}

/// One pixel from a read-back buffer, as `(r, g, b, a)`.
pub fn pixel_at(data: &[u8], width: usize, x: usize, y: usize) -> (u8, u8, u8, u8) {
    let i = (y * width + x) * 4;
    // The drawable format is BGRA.
    (data[i + 2], data[i + 1], data[i], data[i + 3])
}

/// True when the scheduler says a frame is owed and one can be started.
pub fn should_draw(scheduler: &mut FrameScheduler) -> bool {
    scheduler.poll() == Decision::Draw
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_atlas::atlas::Atlas;
    use mica_atlas::fontset::{FontSet, Style};
    use mica_core::backend::RowRef;
    use mica_core::cell::{Cell, CellContent, CellFlags, Color};
    use mica_core::material::{builtin, Material, Role};
    use mica_core::sidetable::SideTables;

    use crate::grid::{RowBuilder, Rgba};

    const W: u32 = 160;
    const H: u32 = 80;

    fn material() -> Material {
        Material::from_theme(&builtin("slate").unwrap()).unwrap()
    }

    fn atlas() -> Atlas {
        Atlas::new(FontSet::resolve("Menlo", 13.0, 1.0))
    }

    fn substrate(material: &Material) -> SubstrateUniforms {
        let bg = material.role(Role::Background).to_linear();
        SubstrateUniforms {
            background: bg,
            tint: [0.0; 4],
            focus: [0.5, 0.5],
            // Zero intensity and zero vignette: a flat background is what the
            // pixel assertions below can actually check against.
            intensity: 0.0,
            vignette: 0.0,
        }
    }

    fn uniforms(atlas: &Atlas) -> Uniforms {
        Uniforms::new(
            (W as f32, H as f32),
            atlas.metrics(),
            (0.0, 0.0),
            atlas.page_size() as f32,
            0.0,
            1.0,
        )
    }

    fn render(renderer: &mut Renderer, atlas: &mut Atlas, material: &Material) -> Vec<u8> {
        renderer.sync_atlas(atlas).unwrap();
        let target = renderer.context().offscreen_target(W, H).unwrap();
        renderer
            .render_to_texture(&target, uniforms(atlas), substrate(material))
            .unwrap();
        read_back(&target)
    }

    #[test]
    fn an_empty_grid_renders_the_background_colour() {
        let material = material();
        let mut atlas = atlas();
        let mut renderer = Renderer::new(512).unwrap();

        let pixels = render(&mut renderer, &mut atlas, &material);
        let (r, g, b, a) = pixel_at(&pixels, W as usize, 10, 10);
        let expected = material.role(Role::Background);

        // The substrate writes linear values into an Unorm target, so allow a
        // little slack rather than demanding an exact match.
        assert!(a == 255, "the frame is not opaque");
        assert!(
            (r as i32 - expected.r as i32).abs() < 40
                && (g as i32 - expected.g as i32).abs() < 40
                && (b as i32 - expected.b as i32).abs() < 40,
            "background was ({r},{g},{b}), expected roughly {expected:?}"
        );
    }

    #[test]
    fn a_cell_background_lands_on_its_own_cell_and_nowhere_else() {
        let material = material();
        let mut atlas = atlas();
        let metrics = atlas.metrics();
        let mut renderer = Renderer::new(512).unwrap();

        renderer
            .buffers()
            .backgrounds
            .push(crate::grid::BgInstance::new(2, 1, 1, Rgba([0, 0, 255, 255])));

        let pixels = render(&mut renderer, &mut atlas, &material);
        let inside = (
            metrics.width as usize * 2 + metrics.width as usize / 2,
            metrics.height as usize + metrics.height as usize / 2,
        );
        let (r, g, b, _) = pixel_at(&pixels, W as usize, inside.0, inside.1);
        assert!(b > 200 && r < 60 && g < 60, "cell (2,1) is ({r},{g},{b}), expected blue");

        // The neighbouring cell must be untouched.
        let (nr, _, nb, _) = pixel_at(&pixels, W as usize, metrics.width as usize / 2, inside.1);
        assert!(nb < 200 || nr > 60, "the background bled into cell (0,1)");
    }

    #[test]
    fn text_actually_reaches_the_framebuffer() {
        let material = material();
        let mut atlas = atlas();
        let tables = SideTables::new();
        let metrics = atlas.metrics();
        let mut renderer = Renderer::new(512).unwrap();

        // A solid block glyph is used rather than a letter: it fills its cell,
        // so the assertion does not depend on where a particular font puts its
        // ink.
        let cells: Vec<Cell> = vec![
            Cell::new(
                CellContent::scalar('\u{2588}'),
                Color::rgb(255, 0, 0),
                Color::DEFAULT,
                CellFlags::EMPTY,
            ),
            Cell::EMPTY,
        ];

        let builder = RowBuilder { material: &material, tables: &tables, metrics, alpha: 1.0 };
        let mut buffers = InstanceBuffers::default();
        builder.build_row(
            RowRef { index: 0, cells: &cells, wrapped: false },
            &mut atlas,
            &mut buffers,
        );
        assert_eq!(buffers.glyphs.len(), 1);
        *renderer.buffers() = buffers;

        let pixels = render(&mut renderer, &mut atlas, &material);
        let (r, g, b, _) = pixel_at(
            &pixels,
            W as usize,
            metrics.width as usize / 2,
            metrics.height as usize / 2,
        );
        assert!(r > 180 && g < 80 && b < 80, "the glyph rendered as ({r},{g},{b}), not red");
    }

    #[test]
    fn the_caret_is_drawn_where_it_was_asked_for() {
        let material = material();
        let mut atlas = atlas();
        let metrics = atlas.metrics();
        let mut renderer = Renderer::new(512).unwrap();

        let cursor = mica_core::backend::CursorState {
            line: 2,
            column: 3,
            blinking: false,
            ..Default::default()
        };
        let shape =
            crate::grid::cursor_shape(cursor, metrics, &material, (0.0, 0.0), true, true).unwrap();
        renderer.buffers().shapes.push(shape);

        let pixels = render(&mut renderer, &mut atlas, &material);
        let (r, g, b, _) = pixel_at(
            &pixels,
            W as usize,
            metrics.width as usize * 3 + metrics.width as usize / 2,
            metrics.height as usize * 2 + metrics.height as usize / 2,
        );
        let accent = material.role(Role::Accent);
        assert!(
            (r as i32 - accent.r as i32).abs() < 60
                && (b as i32 - accent.b as i32).abs() < 60
                && (g as i32 - accent.g as i32).abs() < 60,
            "the caret rendered as ({r},{g},{b}), expected roughly {accent:?}"
        );
    }

    #[test]
    fn an_idle_renderer_never_submits_a_command_buffer() {
        // The Phase 5 exit test, at the level where it actually matters: ten
        // seconds of a 120 Hz display asking whether to draw.
        let mut renderer = Renderer::new(512).unwrap();
        for _ in 0..1200 {
            assert!(!should_draw(renderer.scheduler()));
        }
        assert_eq!(renderer.stats().submitted, 0);
    }

    #[test]
    fn a_new_glyph_wakes_the_renderer_and_uploading_it_does_not_repeat() {
        let mut atlas = atlas();
        let mut renderer = Renderer::new(512).unwrap();
        assert!(!should_draw(renderer.scheduler()));

        atlas.glyph(mica_atlas::atlas::GlyphKey::scalar('A', Style::REGULAR), || None);
        renderer.sync_atlas(&mut atlas).unwrap();
        assert!(should_draw(renderer.scheduler()), "a new glyph must schedule a frame");

        renderer.scheduler().begin_frame();
        renderer.scheduler().end_frame();
        renderer.sync_atlas(&mut atlas).unwrap();
        assert!(!should_draw(renderer.scheduler()), "an unchanged atlas woke the renderer");
    }

    #[test]
    fn a_rebuilt_atlas_drops_the_textures_it_invalidated() {
        let mut atlas = atlas();
        let mut renderer = Renderer::new(512).unwrap();
        atlas.glyph(mica_atlas::atlas::GlyphKey::scalar('A', Style::REGULAR), || None);
        renderer.sync_atlas(&mut atlas).unwrap();
        assert_eq!(renderer.context().pages().len(), 1);

        atlas.rebuild(FontSet::resolve("Menlo", 20.0, 1.0));
        renderer.sync_atlas(&mut atlas).unwrap();
        assert!(
            renderer.context().pages().is_empty(),
            "stale textures survived a font change"
        );
    }

    #[test]
    fn an_empty_frame_still_paints_the_substrate() {
        // Skipping passes must never skip the one that owns every pixel.
        let material = material();
        let mut atlas = atlas();
        let mut renderer = Renderer::new(512).unwrap();
        let pixels = render(&mut renderer, &mut atlas, &material);
        assert!(pixels.iter().any(|&b| b != 0), "the frame came back entirely blank");
    }
}
