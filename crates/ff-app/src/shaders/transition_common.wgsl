// Shared header for all transitions. Each transition file defines
//
//     fn transition(uv: vec2<f32>, p: f32) -> vec4<f32>
//
// blending `get_from(uv)` (outgoing image) and `get_to(uv)` (incoming image)
// by the progress `p` in 0..1. Colours are premultiplied. Adding a new
// transition = adding one .wgsl file and listing it in canvas.rs.

struct Params {
    progress: f32,
    aspect: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var tex_from: texture_2d<f32>;
@group(0) @binding(3) var tex_to: texture_2d<f32>;

fn inside(uv: vec2<f32>) -> f32 {
    let s = step(vec2<f32>(0.0), uv) * step(uv, vec2<f32>(1.0));
    return s.x * s.y;
}

fn get_from(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_from, samp, uv, 0.0) * inside(uv);
}

fn get_to(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_to, samp, uv, 0.0) * inside(uv);
}

fn ease(p: f32) -> f32 {
    return p * p * (3.0 - 2.0 * p);
}

fn hash21(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    // One oversized triangle covering the viewport.
    var out: VsOut;
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return transition(in.uv, clamp(params.progress, 0.0, 1.0));
}
