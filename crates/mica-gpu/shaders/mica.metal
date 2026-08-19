//
//  mica.metal — every pipeline Mica draws with.
//
//  Nine vertex/fragment pairs, in the order they run:
//
//    1. substrate     ambient light behind everything
//    2. cell_bg       per-cell background quads
//    3. cell          glyphs from the atlas
//    4. cell_rule     underline, strikethrough, overline
//    5. block_gutter  OSC 133 command-block marks
//    6. shape         the caret
//    7. decay         the caret's trail
//    8. quad          rounded chrome for overlays
//    9. ui_text       chrome text
//
//  `ui_text` is a separate pipeline from `cell` on purpose. The palette, the
//  find bar, and the status bar are drawn *inside* the Metal layer rather than
//  as AppKit views over it. That is what lets the palette open within one frame
//  — there is no view to instantiate, lay out, and composite — and it is a
//  decision that cannot be retrofitted without rewriting the overlay layer.
//
//  Every pipeline is instanced from the same unit quad. There is no vertex
//  buffer: `vertex_id` indexes a corner and `instance_id` indexes the thing
//  being drawn, so the only per-frame upload is the instance array.
//

#include <metal_stdlib>
using namespace metal;

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

struct Uniforms {
    float2 viewport;      // drawable size, device pixels
    float2 cell;          // cell size, device pixels
    float2 origin;        // top-left of the grid within the drawable
    float2 atlas_size;    // glyph page dimensions, texels
    float  time;          // seconds since launch, for animation only
    float  alpha;         // global fade, used by the theme cross-fade
    float2 _pad;
};

/// The four corners of a unit quad, as a triangle strip.
static inline float2 unit_corner(uint vertex_id) {
    // 0:(0,0) 1:(1,0) 2:(0,1) 3:(1,1)
    return float2(float(vertex_id & 1u), float((vertex_id >> 1) & 1u));
}

/// Pixel space (y down, origin top-left) to clip space.
static inline float4 to_clip(float2 pixel, float2 viewport) {
    float2 ndc = pixel / viewport * 2.0 - 1.0;
    return float4(ndc.x, -ndc.y, 0.0, 1.0);
}

static inline float4 unpack_color(uchar4 c) {
    return float4(c) / 255.0;
}

struct Varyings {
    float4 position [[position]];
    float4 color;
    float2 uv;
};

// ---------------------------------------------------------------------------
// 1. substrate — the ambient light pass
// ---------------------------------------------------------------------------
//
// One full-screen quad. Not a decoration: it is what stops a flat background
// from reading as a dead rectangle, and it costs one triangle strip.

struct SubstrateUniforms {
    float4 background;
    float4 tint;
    float2 focus;     // where the light sits, in normalised drawable space
    float  intensity;
    float  vignette;
};

struct SubstrateVaryings {
    float4 position [[position]];
    float2 uv;
};

vertex SubstrateVaryings substrate_vertex(uint vid [[vertex_id]],
                                          constant Uniforms& u [[buffer(0)]]) {
    float2 corner = unit_corner(vid);
    SubstrateVaryings out;
    out.position = to_clip(corner * u.viewport, u.viewport);
    out.uv = corner;
    return out;
}

fragment float4 substrate_fragment(SubstrateVaryings in [[stage_in]],
                                   constant SubstrateUniforms& s [[buffer(0)]]) {
    float2 delta = in.uv - s.focus;
    // Aspect-correct so the falloff is circular rather than stretched.
    float distance = length(delta);
    float light = exp(-distance * distance * 2.0) * s.intensity;
    float vignette = 1.0 - s.vignette * smoothstep(0.35, 1.0, distance);
    float3 rgb = (s.background.rgb + s.tint.rgb * light) * vignette;
    return float4(rgb, 1.0);
}

// ---------------------------------------------------------------------------
// 2. cell_bg — per-cell background
// ---------------------------------------------------------------------------

struct BgInstance {
    ushort2 cell;      // column, row
    ushort  width;     // in cells: 1 or 2
    ushort  _pad;
    uchar4  color;
};

vertex Varyings cell_bg_vertex(uint vid [[vertex_id]],
                               uint iid [[instance_id]],
                               constant Uniforms& u [[buffer(0)]],
                               const device BgInstance* instances [[buffer(1)]]) {
    BgInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float2 size = float2(u.cell.x * float(inst.width), u.cell.y);
    float2 pixel = u.origin + float2(inst.cell) * u.cell + corner * size;

    Varyings out;
    out.position = to_clip(pixel, u.viewport);
    out.color = unpack_color(inst.color);
    out.uv = corner;
    return out;
}

fragment float4 cell_bg_fragment(Varyings in [[stage_in]]) {
    return in.color;
}

// ---------------------------------------------------------------------------
// 3. cell — glyphs
// ---------------------------------------------------------------------------

struct GlyphInstance {
    ushort2 cell;
    short2  offset;     // glyph bearing within the cell, device pixels
    ushort2 size;       // glyph size, texels
    ushort2 uv_origin;  // top-left in the atlas page
    uchar4  color;
    ushort  page;
    ushort  flags;      // bit 0: colour glyph (sample as BGRA, do not tint)
};

constant ushort GLYPH_FLAG_COLOR = 1;

struct GlyphVaryings {
    float4 position [[position]];
    float4 color;
    float2 uv;
    ushort flags;
};

vertex GlyphVaryings cell_vertex(uint vid [[vertex_id]],
                                 uint iid [[instance_id]],
                                 constant Uniforms& u [[buffer(0)]],
                                 const device GlyphInstance* instances [[buffer(1)]]) {
    GlyphInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float2 size = float2(inst.size);
    float2 cell_origin = u.origin + float2(inst.cell) * u.cell;
    float2 pixel = cell_origin + float2(inst.offset) + corner * size;

    GlyphVaryings out;
    out.position = to_clip(pixel, u.viewport);
    out.color = unpack_color(inst.color);
    out.uv = (float2(inst.uv_origin) + corner * size) / u.atlas_size;
    out.flags = inst.flags;
    return out;
}

fragment float4 cell_fragment(GlyphVaryings in [[stage_in]],
                              texture2d<float> mask [[texture(0)]],
                              texture2d<float> color_page [[texture(1)]],
                              sampler s [[sampler(0)]]) {
    if (in.flags & GLYPH_FLAG_COLOR) {
        // An emoji is not tintable. Sampling it and multiplying by the cell's
        // foreground would turn a colour glyph into a coloured blob, which is
        // exactly the failure this branch exists to avoid.
        float4 texel = color_page.sample(s, in.uv);
        return float4(texel.rgb, texel.a);
    }
    float coverage = mask.sample(s, in.uv).r;
    return float4(in.color.rgb * in.color.a, in.color.a) * coverage;
}

// ---------------------------------------------------------------------------
// 4. cell_rule — underline, strikethrough, overline
// ---------------------------------------------------------------------------
//
// A separate pass rather than part of the glyph, because the rule's colour is
// independent of the text's — that is the whole point of per-cell underline
// colour — and because a curly underline is a shape, not a glyph.

struct RuleInstance {
    ushort2 cell;
    ushort  width;      // in cells
    ushort  style;      // 0 none, 1 single, 2 double, 3 curly, 4 dotted, 5 dashed
    short   offset;     // device pixels from the top of the cell
    ushort  thickness;
    uchar4  color;
};

struct RuleVaryings {
    float4 position [[position]];
    float4 color;
    float2 local;       // pixels within the rule's own box
    float2 extent;
    ushort style;
    float  thickness;
};

vertex RuleVaryings cell_rule_vertex(uint vid [[vertex_id]],
                                     uint iid [[instance_id]],
                                     constant Uniforms& u [[buffer(0)]],
                                     const device RuleInstance* instances [[buffer(1)]]) {
    RuleInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float t = float(inst.thickness);
    // A curly underline needs vertical room for its amplitude; give every
    // style the same box so the pipeline has one geometry path.
    float box_height = (inst.style == 3) ? t * 3.0 : t;
    float2 size = float2(u.cell.x * float(inst.width), box_height);
    float2 pixel = u.origin + float2(inst.cell) * u.cell
                 + float2(0.0, float(inst.offset)) + corner * size;

    RuleVaryings out;
    out.position = to_clip(pixel, u.viewport);
    out.color = unpack_color(inst.color);
    out.local = corner * size;
    out.extent = size;
    out.style = inst.style;
    out.thickness = t;
    return out;
}

fragment float4 cell_rule_fragment(RuleVaryings in [[stage_in]]) {
    float alpha = 1.0;
    switch (in.style) {
        case 2: { // double
            float band = in.extent.y / 3.0;
            bool upper = in.local.y < band;
            bool lower = in.local.y > band * 2.0;
            alpha = (upper || lower) ? 1.0 : 0.0;
            break;
        }
        case 3: { // curly
            float amplitude = in.extent.y * 0.5 - in.thickness * 0.5;
            float centre = in.extent.y * 0.5;
            float wave = centre + sin(in.local.x / in.thickness * 1.6) * amplitude;
            float distance = abs(in.local.y - wave);
            alpha = 1.0 - smoothstep(in.thickness * 0.5 - 0.5,
                                     in.thickness * 0.5 + 0.5, distance);
            break;
        }
        case 4: { // dotted
            float period = in.thickness * 2.0;
            alpha = fmod(in.local.x, period) < in.thickness ? 1.0 : 0.0;
            break;
        }
        case 5: { // dashed
            float period = in.thickness * 6.0;
            alpha = fmod(in.local.x, period) < period * 0.6 ? 1.0 : 0.0;
            break;
        }
        default:
            break;
    }
    return float4(in.color.rgb * in.color.a, in.color.a) * alpha;
}

// ---------------------------------------------------------------------------
// 5. block_gutter — OSC 133 command-block marks
// ---------------------------------------------------------------------------
//
// The narrow strip left of the grid. A failed command keeps its mark until the
// block is cleared, which is why the gutter is drawn from block state rather
// than from cell state.

struct GutterInstance {
    ushort  row;
    ushort  rows;
    uchar4  color;
    float   width;      // device pixels
    float   radius;
};

struct GutterVaryings {
    float4 position [[position]];
    float4 color;
    float2 local;
    float2 extent;
    float  radius;
};

vertex GutterVaryings block_gutter_vertex(uint vid [[vertex_id]],
                                          uint iid [[instance_id]],
                                          constant Uniforms& u [[buffer(0)]],
                                          const device GutterInstance* instances [[buffer(1)]]) {
    GutterInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float2 size = float2(inst.width, u.cell.y * float(inst.rows));
    float2 pixel = float2(u.origin.x - inst.width * 2.0,
                          u.origin.y + float(inst.row) * u.cell.y) + corner * size;

    GutterVaryings out;
    out.position = to_clip(pixel, u.viewport);
    out.color = unpack_color(inst.color);
    out.local = corner * size;
    out.extent = size;
    out.radius = inst.radius;
    return out;
}

/// Signed distance to a rounded box, centred at the origin.
static inline float rounded_box_sdf(float2 point, float2 half_size, float radius) {
    float2 q = abs(point) - half_size + radius;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

fragment float4 block_gutter_fragment(GutterVaryings in [[stage_in]]) {
    float2 centred = in.local - in.extent * 0.5;
    float distance = rounded_box_sdf(centred, in.extent * 0.5, in.radius);
    float alpha = 1.0 - smoothstep(-0.5, 0.5, distance);
    return float4(in.color.rgb * in.color.a, in.color.a) * alpha;
}

// ---------------------------------------------------------------------------
// 6. shape — the caret
// ---------------------------------------------------------------------------

// Field order and explicit padding are deliberate throughout: MSL aligns
// `float2` to 8 bytes and `uchar4`/`ushort2` to 4, so laying the wide fields
// out first and padding to a multiple of the struct alignment is what keeps
// these byte-identical to their `#[repr(C)]` twins in `grid.rs`. Those twins
// assert their own sizes at compile time.
struct ShapeInstance {
    float2 origin;      // device pixels
    float2 size;
    uchar4 color;
    float  radius;
    float  softness;    // 0 hard-edged, 1 fully smeared — the motion styles
    float  _pad;
};

struct ShapeVaryings {
    float4 position [[position]];
    float4 color;
    float2 local;
    float2 extent;
    float  radius;
    float  softness;
};

vertex ShapeVaryings shape_vertex(uint vid [[vertex_id]],
                                  uint iid [[instance_id]],
                                  constant Uniforms& u [[buffer(0)]],
                                  const device ShapeInstance* instances [[buffer(1)]]) {
    ShapeInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float2 pixel = inst.origin + corner * inst.size;

    ShapeVaryings out;
    out.position = to_clip(pixel, u.viewport);
    out.color = unpack_color(inst.color);
    out.local = corner * inst.size;
    out.extent = inst.size;
    out.radius = inst.radius;
    out.softness = inst.softness;
    return out;
}

fragment float4 shape_fragment(ShapeVaryings in [[stage_in]]) {
    float2 centred = in.local - in.extent * 0.5;
    float distance = rounded_box_sdf(centred, in.extent * 0.5, in.radius);
    float feather = 0.5 + in.softness * min(in.extent.x, in.extent.y) * 0.5;
    float alpha = 1.0 - smoothstep(-feather, feather, distance);
    return float4(in.color.rgb * in.color.a, in.color.a) * alpha;
}

// ---------------------------------------------------------------------------
// 7. decay — the caret's trail
// ---------------------------------------------------------------------------
//
// Same geometry as `shape`, but the alpha falls off along the direction of
// travel so a fast caret leaves a wake rather than a row of ghosts.

struct DecayInstance {
    float2 origin;
    float2 size;
    float2 direction;   // normalised
    uchar4 color;
    float  age;         // 0 just emitted, 1 gone
    float  radius;
    float  _pad;
};

struct DecayVaryings {
    float4 position [[position]];
    float4 color;
    float2 local;
    float2 extent;
    float2 direction;
    float  age;
    float  radius;
};

vertex DecayVaryings decay_vertex(uint vid [[vertex_id]],
                                  uint iid [[instance_id]],
                                  constant Uniforms& u [[buffer(0)]],
                                  const device DecayInstance* instances [[buffer(1)]]) {
    DecayInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float2 pixel = inst.origin + corner * inst.size;

    DecayVaryings out;
    out.position = to_clip(pixel, u.viewport);
    out.color = unpack_color(inst.color);
    out.local = corner * inst.size;
    out.extent = inst.size;
    out.direction = inst.direction;
    out.age = inst.age;
    out.radius = inst.radius;
    return out;
}

fragment float4 decay_fragment(DecayVaryings in [[stage_in]]) {
    float2 centred = in.local - in.extent * 0.5;
    float distance = rounded_box_sdf(centred, in.extent * 0.5, in.radius);
    float shape_alpha = 1.0 - smoothstep(-1.0, 1.0, distance);

    // Fade along the trail: fully opaque at the leading edge, gone at the tail.
    float2 normalised = centred / max(in.extent * 0.5, float2(1.0));
    float along = dot(normalised, in.direction) * 0.5 + 0.5;
    float trail = mix(0.15, 1.0, along);

    float alpha = shape_alpha * trail * (1.0 - in.age);
    return float4(in.color.rgb * in.color.a, in.color.a) * alpha;
}

// ---------------------------------------------------------------------------
// 8. quad — rounded chrome
// ---------------------------------------------------------------------------

struct QuadInstance {
    float2 origin;
    float2 size;
    uchar4 fill;
    uchar4 border;
    float  radius;
    float  border_width;
};

struct QuadVaryings {
    float4 position [[position]];
    float4 fill;
    float4 border;
    float2 local;
    float2 extent;
    float  radius;
    float  border_width;
};

vertex QuadVaryings quad_vertex(uint vid [[vertex_id]],
                                uint iid [[instance_id]],
                                constant Uniforms& u [[buffer(0)]],
                                const device QuadInstance* instances [[buffer(1)]]) {
    QuadInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float2 pixel = inst.origin + corner * inst.size;

    QuadVaryings out;
    out.position = to_clip(pixel, u.viewport);
    out.fill = unpack_color(inst.fill);
    out.border = unpack_color(inst.border);
    out.local = corner * inst.size;
    out.extent = inst.size;
    out.radius = inst.radius;
    out.border_width = inst.border_width;
    return out;
}

fragment float4 quad_fragment(QuadVaryings in [[stage_in]]) {
    float2 centred = in.local - in.extent * 0.5;
    float distance = rounded_box_sdf(centred, in.extent * 0.5, in.radius);
    float outer = 1.0 - smoothstep(-0.5, 0.5, distance);
    float inner = 1.0 - smoothstep(-0.5, 0.5, distance + in.border_width);

    float4 colour = mix(in.border, in.fill, inner);
    float alpha = outer * colour.a;
    return float4(colour.rgb * alpha, alpha);
}

// ---------------------------------------------------------------------------
// 9. ui_text — chrome text
// ---------------------------------------------------------------------------
//
// Same atlas, different geometry: chrome text is positioned in free pixels
// rather than snapped to the grid, so it can be centred in a palette row.

struct UiTextInstance {
    float2 origin;      // device pixels
    ushort2 size;
    ushort2 uv_origin;
    uchar4  color;
    ushort  page;
    ushort  flags;
};

vertex GlyphVaryings ui_text_vertex(uint vid [[vertex_id]],
                                    uint iid [[instance_id]],
                                    constant Uniforms& u [[buffer(0)]],
                                    const device UiTextInstance* instances [[buffer(1)]]) {
    UiTextInstance inst = instances[iid];
    float2 corner = unit_corner(vid);
    float2 size = float2(inst.size);
    float2 pixel = inst.origin + corner * size;

    GlyphVaryings out;
    out.position = to_clip(pixel, u.viewport);
    out.color = unpack_color(inst.color);
    out.color.a *= u.alpha;
    out.uv = (float2(inst.uv_origin) + corner * size) / u.atlas_size;
    out.flags = inst.flags;
    return out;
}

fragment float4 ui_text_fragment(GlyphVaryings in [[stage_in]],
                                 texture2d<float> mask [[texture(0)]],
                                 texture2d<float> color_page [[texture(1)]],
                                 sampler s [[sampler(0)]]) {
    if (in.flags & GLYPH_FLAG_COLOR) {
        float4 texel = color_page.sample(s, in.uv);
        return float4(texel.rgb, texel.a) * in.color.a;
    }
    float coverage = mask.sample(s, in.uv).r;
    return float4(in.color.rgb * in.color.a, in.color.a) * coverage;
}
