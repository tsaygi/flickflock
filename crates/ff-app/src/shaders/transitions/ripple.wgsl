// Concentric waves distort both images while they cross-fade.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let d = (uv - vec2<f32>(0.5)) * vec2<f32>(params.aspect, 1.0);
    let dist = length(d);
    let dir = d / max(dist, 0.0001);
    let strength = sin(p * 3.14159265) * 0.03;
    let wave = sin(dist * 40.0 - p * 18.0) * strength;
    let offset = dir * wave / vec2<f32>(params.aspect, 1.0);
    return mix(get_from(uv + offset), get_to(uv + offset), ease(p));
}
