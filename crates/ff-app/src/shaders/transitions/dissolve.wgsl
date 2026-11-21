// Random blocks flip from the old image to the new one.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let cells = vec2<f32>(96.0 * params.aspect, 96.0);
    let n = hash21(floor(uv * cells));
    let m = smoothstep(n - 0.08, n + 0.08, p * 1.16 - 0.08);
    return mix(get_from(uv), get_to(uv), m);
}
