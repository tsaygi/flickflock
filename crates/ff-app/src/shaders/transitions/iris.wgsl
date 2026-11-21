// A circle opening from the centre.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let d = (uv - vec2<f32>(0.5)) * vec2<f32>(params.aspect, 1.0);
    let max_r = length(vec2<f32>(params.aspect, 1.0)) * 0.5;
    let r = ease(p) * (max_r + 0.04);
    let m = 1.0 - smoothstep(r - 0.02, r + 0.02, length(d));
    return mix(get_from(uv), get_to(uv), m);
}
