// Draws image tiles (premultiplied RGBA) with zoom/pan applied on the CPU:
// vertex positions arrive in viewport pixels.

struct Globals {
    viewport: vec2<f32>,
    opacity: f32,
    checker: f32,
    checker_size: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var samp: sampler;
@group(1) @binding(0) var tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) local: vec2<f32>) -> VsOut {
    var out: VsOut;
    let ndc = pos / g.viewport * 2.0 - vec2<f32>(1.0, 1.0);
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = uv;
    out.local = local;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    var c = textureSample(tex, samp, in.uv);
    if g.checker > 0.5 {
        // Checkerboard behind transparent pixels, anchored to the image.
        let cell = floor(in.local / g.checker_size);
        let odd = abs(cell.x + cell.y) % 2.0;
        let bg = mix(0.78, 0.94, odd);
        c = vec4<f32>(c.rgb + vec3<f32>(bg) * (1.0 - c.a), 1.0);
    }
    return c * g.opacity;
}
