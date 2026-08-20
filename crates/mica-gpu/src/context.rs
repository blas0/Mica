//! The Metal device, the nine pipelines, and the atlas textures.
//!
//! Everything here is created once. A frame touches this module only to look
//! up a pipeline and to blit new glyphs, which is why the per-frame cost of the
//! renderer is instance data and nothing else.
//!
//! There is no window in this file. A `CAMetalLayer` is passed in from
//! `mica-shell` at draw time, and the whole render path can also target an
//! offscreen texture — which is what lets the pixels be asserted on in tests
//! rather than looked at in a screenshot.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSString, NSURL};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder,
    MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLPixelFormat,
    MTLPrimitiveType, MTLRenderPipelineDescriptor, MTLRenderPipelineState,
    MTLResourceOptions, MTLSamplerAddressMode, MTLSamplerDescriptor, MTLSamplerMinMagFilter,
    MTLSamplerState, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
    MTLBlendFactor, MTLBlendOperation, MTLOrigin, MTLResource,
};

use mica_atlas::raster::PixelFormat;

/// The drawable's pixel format. BGRA8 with sRGB is what `CAMetalLayer` gives
/// by default on macOS, and matching it avoids a conversion pass.
pub const DRAWABLE_FORMAT: MTLPixelFormat = MTLPixelFormat::BGRA8Unorm;

/// The nine pipelines, in draw order.
///
/// Named rather than indexed so a frame capture and a stack trace both say
/// which pass is which.
pub struct Pipelines {
    pub substrate: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub cell_bg: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub cell: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub cell_rule: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub block_gutter: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub shape: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub decay: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub quad: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub ui_text: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

#[derive(Debug)]
pub enum GpuError {
    NoDevice,
    NoQueue,
    LibraryNotFound(String),
    LibraryFailed(String),
    MissingFunction(&'static str),
    PipelineFailed { name: &'static str, message: String },
    TextureFailed,
    BufferFailed,
    SamplerFailed,
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuError::NoDevice => write!(f, "no Metal device (Mica requires Apple Silicon)"),
            GpuError::NoQueue => write!(f, "could not create a Metal command queue"),
            GpuError::LibraryNotFound(p) => write!(f, "default.metallib not found at {p}"),
            GpuError::LibraryFailed(m) => write!(f, "could not load default.metallib: {m}"),
            GpuError::MissingFunction(n) => write!(f, "shader function `{n}` is missing"),
            GpuError::PipelineFailed { name, message } => {
                write!(f, "pipeline `{name}` failed to build: {message}")
            }
            GpuError::TextureFailed => write!(f, "could not create a Metal texture"),
            GpuError::BufferFailed => write!(f, "could not create a Metal buffer"),
            GpuError::SamplerFailed => write!(f, "could not create a Metal sampler"),
        }
    }
}

impl std::error::Error for GpuError {}

/// One atlas page's texture.
pub struct PageTexture {
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    pub format: PixelFormat,
}

pub struct GpuContext {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipelines: Pipelines,
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    pages: Vec<PageTexture>,
    page_size: u32,
    atlas_generation: u32,
    /// Glyph uploads staged for the next command buffer.
    ///
    /// See [`GpuContext::stage_upload`] for why they are staged rather than
    /// written straight into the texture.
    pending: Vec<StagedUpload>,
}

/// One glyph rectangle, waiting to be blitted into its page.
struct StagedUpload {
    page: usize,
    origin: MTLOrigin,
    size: MTLSize,
    bytes_per_row: usize,
    source: Retained<ProtocolObject<dyn objc2_metal::MTLBuffer>>,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("device", &self.device_name())
            .field("pages", &self.pages.len())
            .field("page_size", &self.page_size)
            .finish_non_exhaustive()
    }
}

impl GpuContext {
    pub fn new(page_size: u16) -> Result<GpuContext, GpuError> {
        let device = MTLCreateSystemDefaultDevice().ok_or(GpuError::NoDevice)?;
        let queue = device.newCommandQueue().ok_or(GpuError::NoQueue)?;
        queue.setLabel(Some(&NSString::from_str("mica.queue")));

        let library = load_library(&device)?;
        let pipelines = build_pipelines(&device, &library)?;
        let sampler = build_sampler(&device)?;

        Ok(GpuContext {
            device,
            queue,
            pipelines,
            sampler,
            pages: Vec::new(),
            page_size: page_size.max(64) as u32,
            atlas_generation: u32::MAX,
            pending: Vec::new(),
        })
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    pub fn queue(&self) -> &ProtocolObject<dyn MTLCommandQueue> {
        &self.queue
    }

    pub fn pipelines(&self) -> &Pipelines {
        &self.pipelines
    }

    pub fn sampler(&self) -> &ProtocolObject<dyn MTLSamplerState> {
        &self.sampler
    }

    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    pub fn pages(&self) -> &[PageTexture] {
        &self.pages
    }

    pub fn device_name(&self) -> String {
        self.device.name().to_string()
    }

    /// Drops every atlas texture. Called when the atlas reports a new
    /// generation — a font or size change, or a page eviction.
    pub fn reset_atlas(&mut self, generation: u32) {
        self.pages.clear();
        self.atlas_generation = generation;
    }

    pub fn atlas_generation(&self) -> u32 {
        self.atlas_generation
    }

    /// Stages one glyph for its page, creating the page's texture on first use.
    ///
    /// **Staged rather than written directly, and that is the whole point.**
    /// The obvious implementation is `replaceRegion`, a CPU write straight into
    /// the texture — and it is correct exactly as long as the GPU is not
    /// reading that texture at the time. While the renderer waited for every
    /// frame to complete, that was guaranteed by accident. It is not guaranteed
    /// any more: a frame is committed and returns immediately, so the previous
    /// frame can still be sampling the atlas when the next glyph arrives.
    /// Typing is precisely when new glyphs arrive, which is precisely when the
    /// corruption showed.
    ///
    /// Copying through a buffer puts the write on the GPU's own timeline, where
    /// Metal's automatic hazard tracking orders it against the reads.
    ///
    /// Pages are created lazily because a session that never draws an emoji
    /// should never allocate a four-byte-per-pixel texture.
    pub fn stage_upload(&mut self, upload: &mica_atlas::atlas::Upload) -> Result<(), GpuError> {
        let index = upload.page as usize;
        while self.pages.len() <= index {
            // A page is only ever appended in order, so any gap would be a bug
            // in the atlas rather than something to paper over here.
            let format = upload.format;
            self.pages.push(self.create_page(format)?);
        }
        if self.pages[index].format != upload.format {
            // The atlas guarantees a page never changes format; if it does,
            // recreating the texture is the only safe response.
            self.pages[index] = self.create_page(upload.format)?;
        }

        let bytes_per_row = upload.rect.width as usize * upload.format.bytes_per_pixel();
        let source = self
            .buffer_from(&upload.data, "mica.atlas.staging")?
            .ok_or(GpuError::TextureFailed)?;

        self.pending.push(StagedUpload {
            page: index,
            origin: MTLOrigin { x: upload.rect.x as usize, y: upload.rect.y as usize, z: 0 },
            size: MTLSize {
                width: upload.rect.width as usize,
                height: upload.rect.height as usize,
                depth: 1,
            },
            bytes_per_row,
            source,
        });
        Ok(())
    }

    pub fn has_pending_uploads(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Encodes every staged upload into `command_buffer`, ahead of the render
    /// pass that will sample them.
    ///
    /// Must be called on the same command buffer as the frame that needs the
    /// glyphs, and before its render encoder: within one command buffer, Metal
    /// orders a blit before a later render pass that reads the same texture.
    pub fn flush_uploads(
        &mut self,
        command_buffer: &ProtocolObject<dyn objc2_metal::MTLCommandBuffer>,
    ) -> Result<(), GpuError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let blit = command_buffer.blitCommandEncoder().ok_or(GpuError::BufferFailed)?;
        for staged in self.pending.drain(..) {
            let texture = &self.pages[staged.page].texture;
            unsafe {
                blit.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                    &staged.source,
                    0,
                    staged.bytes_per_row,
                    staged.bytes_per_row * staged.size.height,
                    staged.size,
                    texture,
                    0,
                    0,
                    staged.origin,
                );
            }
        }
        blit.endEncoding();
        Ok(())
    }

    fn create_page(&self, format: PixelFormat) -> Result<PageTexture, GpuError> {
        let pixel_format = match format {
            // R8 rather than A8: sampling `.r` is what the shader does, and R8
            // is the format Metal is happiest blitting into.
            PixelFormat::Alpha8 => MTLPixelFormat::R8Unorm,
            PixelFormat::Bgra8 => MTLPixelFormat::BGRA8Unorm,
        };
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                pixel_format,
                self.page_size as usize,
                self.page_size as usize,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        descriptor.setStorageMode(MTLStorageMode::Managed);

        let texture =
            self.device.newTextureWithDescriptor(&descriptor).ok_or(GpuError::TextureFailed)?;
        texture.setLabel(Some(&NSString::from_str(match format {
            PixelFormat::Alpha8 => "mica.atlas.mask",
            PixelFormat::Bgra8 => "mica.atlas.colour",
        })));
        Ok(PageTexture { texture, format })
    }

    /// An offscreen render target, for tests and for frame captures.
    pub fn offscreen_target(
        &self,
        width: u32,
        height: u32,
    ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, GpuError> {
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                DRAWABLE_FORMAT,
                width as usize,
                height as usize,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        descriptor.setStorageMode(MTLStorageMode::Managed);
        let texture =
            self.device.newTextureWithDescriptor(&descriptor).ok_or(GpuError::TextureFailed)?;
        texture.setLabel(Some(&NSString::from_str("mica.offscreen")));
        Ok(texture)
    }

    /// A buffer holding `data`, labelled so a frame capture is readable.
    ///
    /// Labels cost nothing and turn an unreadable capture into a legible one;
    /// the reference implementation labels every buffer and it is worth
    /// copying.
    pub fn buffer_from<T: Copy>(
        &self,
        data: &[T],
        label: &str,
    ) -> Result<Option<Retained<ProtocolObject<dyn objc2_metal::MTLBuffer>>>, GpuError> {
        if data.is_empty() {
            return Ok(None);
        }
        let bytes = std::mem::size_of_val(data);
        // SAFETY: `data` is valid for `bytes` and Metal copies it immediately.
        let buffer = unsafe {
            self.device.newBufferWithBytes_length_options(
                std::ptr::NonNull::new(data.as_ptr() as *mut std::ffi::c_void)
                    .ok_or(GpuError::BufferFailed)?,
                bytes,
                MTLResourceOptions::StorageModeShared,
            )
        }
        .ok_or(GpuError::BufferFailed)?;
        buffer.setLabel(Some(&NSString::from_str(label)));
        Ok(Some(buffer))
    }
}

/// The primitive every pipeline draws: one instanced triangle strip.
pub const QUAD_PRIMITIVE: MTLPrimitiveType = MTLPrimitiveType::TriangleStrip;
pub const QUAD_VERTEX_COUNT: usize = 4;

fn load_library(
    device: &ProtocolObject<dyn MTLDevice>,
) -> Result<Retained<ProtocolObject<dyn MTLLibrary>>, GpuError> {
    // Two locations, in order: the app bundle when running as `Mica.app`, and
    // the build-time path when running a test or a bare binary.
    let candidates = [
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("../Resources/default.metallib")))
            .map(|p| p.to_string_lossy().into_owned()),
        Some(env!("MICA_METALLIB").to_owned()),
    ];

    let mut last = String::new();
    for candidate in candidates.into_iter().flatten() {
        if !std::path::Path::new(&candidate).exists() {
            last = candidate;
            continue;
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(&candidate));
        match device.newLibraryWithURL_error(&url) {
            Ok(library) => return Ok(library),
            Err(err) => return Err(GpuError::LibraryFailed(err.localizedDescription().to_string())),
        }
    }
    Err(GpuError::LibraryNotFound(last))
}

fn build_sampler(
    device: &ProtocolObject<dyn MTLDevice>,
) -> Result<Retained<ProtocolObject<dyn MTLSamplerState>>, GpuError> {
    let descriptor = MTLSamplerDescriptor::new();
    // Nearest, not linear. Glyphs are rasterised at exactly the size they are
    // drawn, so filtering can only blur them — and at fractional scroll offsets
    // a linear filter is what makes text look soft on one line and crisp on
    // the next.
    descriptor.setMinFilter(MTLSamplerMinMagFilter::Nearest);
    descriptor.setMagFilter(MTLSamplerMinMagFilter::Nearest);
    descriptor.setSAddressMode(MTLSamplerAddressMode::ClampToEdge);
    descriptor.setTAddressMode(MTLSamplerAddressMode::ClampToEdge);
    descriptor.setLabel(Some(&NSString::from_str("mica.sampler")));
    device.newSamplerStateWithDescriptor(&descriptor).ok_or(GpuError::SamplerFailed)
}

fn build_pipelines(
    device: &ProtocolObject<dyn MTLDevice>,
    library: &ProtocolObject<dyn MTLLibrary>,
) -> Result<Pipelines, GpuError> {
    Ok(Pipelines {
        substrate: pipeline(device, library, "substrate", false)?,
        cell_bg: pipeline(device, library, "cell_bg", true)?,
        cell: pipeline(device, library, "cell", true)?,
        cell_rule: pipeline(device, library, "cell_rule", true)?,
        block_gutter: pipeline(device, library, "block_gutter", true)?,
        shape: pipeline(device, library, "shape", true)?,
        decay: pipeline(device, library, "decay", true)?,
        quad: pipeline(device, library, "quad", true)?,
        ui_text: pipeline(device, library, "ui_text", true)?,
    })
}

fn pipeline(
    device: &ProtocolObject<dyn MTLDevice>,
    library: &ProtocolObject<dyn MTLLibrary>,
    name: &'static str,
    blending: bool,
) -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>, GpuError> {
    let vertex = library
        .newFunctionWithName(&NSString::from_str(&format!("{name}_vertex")))
        .ok_or(GpuError::MissingFunction(name))?;
    let fragment = library
        .newFunctionWithName(&NSString::from_str(&format!("{name}_fragment")))
        .ok_or(GpuError::MissingFunction(name))?;

    let descriptor = MTLRenderPipelineDescriptor::new();
    descriptor.setLabel(Some(&NSString::from_str(&format!("mica.{name}"))));
    descriptor.setVertexFunction(Some(&vertex));
    descriptor.setFragmentFunction(Some(&fragment));

    let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    attachment.setPixelFormat(DRAWABLE_FORMAT);
    if blending {
        // Premultiplied source-over. Every fragment shader already multiplies
        // rgb by alpha, so the source factor is One rather than SourceAlpha —
        // getting this pair wrong produces text with dark fringes that looks
        // like a font problem and is not.
        attachment.setBlendingEnabled(true);
        attachment.setRgbBlendOperation(MTLBlendOperation::Add);
        attachment.setAlphaBlendOperation(MTLBlendOperation::Add);
        attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
        attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    }

    device.newRenderPipelineStateWithDescriptor_error(&descriptor).map_err(|err| {
        GpuError::PipelineFailed { name, message: err.localizedDescription().to_string() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_metal::MTLBuffer;

    fn context() -> GpuContext {
        GpuContext::new(512).expect("this machine has no working Metal device")
    }

    #[test]
    fn a_metal_device_and_queue_are_available() {
        let ctx = context();
        assert!(!ctx.device_name().is_empty());
    }

    #[test]
    fn all_nine_pipelines_build() {
        // Every one of these compiles a real shader against a real device, so
        // a typo in `mica.metal` fails here rather than at first paint.
        let ctx = context();
        let p = ctx.pipelines();
        let states = [
            &p.substrate,
            &p.cell_bg,
            &p.cell,
            &p.cell_rule,
            &p.block_gutter,
            &p.shape,
            &p.decay,
            &p.quad,
            &p.ui_text,
        ];
        assert_eq!(states.len(), 9);
        for state in states {
            assert!(!state.label().map(|l| l.to_string()).unwrap_or_default().is_empty());
        }
    }

    #[test]
    fn an_offscreen_target_can_be_created() {
        let ctx = context();
        let texture = ctx.offscreen_target(64, 32).unwrap();
        assert_eq!(texture.width(), 64);
        assert_eq!(texture.height(), 32);
    }

    #[test]
    fn no_atlas_pages_exist_until_a_glyph_is_uploaded() {
        // A session that never draws an emoji must never allocate a colour page.
        let ctx = context();
        assert!(ctx.pages().is_empty());
    }

    #[test]
    fn uploading_a_glyph_creates_its_page_with_the_right_format() {
        use mica_atlas::atlas::Upload;
        use mica_atlas::packer::Rect;

        let mut ctx = context();
        ctx.stage_upload(&Upload {
            page: 0,
            rect: Rect { x: 0, y: 0, width: 4, height: 4 },
            format: PixelFormat::Alpha8,
            data: vec![255u8; 16],
        })
        .unwrap();

        assert_eq!(ctx.pages().len(), 1);
        assert_eq!(ctx.pages()[0].format, PixelFormat::Alpha8);
        assert_eq!(ctx.pages()[0].texture.pixelFormat(), MTLPixelFormat::R8Unorm);
    }

    #[test]
    fn a_colour_glyph_gets_a_bgra_page() {
        use mica_atlas::atlas::Upload;
        use mica_atlas::packer::Rect;

        let mut ctx = context();
        ctx.stage_upload(&Upload {
            page: 0,
            rect: Rect { x: 0, y: 0, width: 2, height: 2 },
            format: PixelFormat::Bgra8,
            data: vec![255u8; 16],
        })
        .unwrap();
        assert_eq!(ctx.pages()[0].texture.pixelFormat(), MTLPixelFormat::BGRA8Unorm);
    }

    #[test]
    fn an_empty_slice_produces_no_buffer_rather_than_an_empty_one() {
        // Metal refuses a zero-length buffer, and a pass with nothing to draw
        // should skip its encoder entirely.
        let ctx = context();
        let none = ctx.buffer_from::<u32>(&[], "mica.empty").unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn a_buffer_carries_its_label_into_a_frame_capture() {
        let ctx = context();
        let buffer = ctx.buffer_from(&[1u32, 2, 3], "mica.frame").unwrap().unwrap();
        assert_eq!(buffer.label().map(|l| l.to_string()).as_deref(), Some("mica.frame"));
        assert_eq!(buffer.length(), 12);
    }

    #[test]
    fn resetting_the_atlas_drops_every_page() {
        use mica_atlas::atlas::Upload;
        use mica_atlas::packer::Rect;

        let mut ctx = context();
        ctx.stage_upload(&Upload {
            page: 0,
            rect: Rect { x: 0, y: 0, width: 2, height: 2 },
            format: PixelFormat::Alpha8,
            data: vec![255u8; 4],
        })
        .unwrap();
        assert_eq!(ctx.pages().len(), 1);

        ctx.reset_atlas(7);
        assert!(ctx.pages().is_empty());
        assert_eq!(ctx.atlas_generation(), 7);
    }
}
