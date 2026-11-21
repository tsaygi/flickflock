// Both images dissolve into big blocks in the middle of the transition.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let amount = 1.0 - abs(p * 2.0 - 1.0);
    let blocks = mix(400.0, 12.0, amount * amount);
    let grid = vec2<f32>(blocks * params.aspect, blocks);
    let snapped = (floor(uv * grid) + vec2<f32>(0.5)) / grid;
    let coord = mix(uv, snapped, step(0.001, amount));
    return mix(get_from(coord), get_to(coord), smoothstep(0.35, 0.65, p));
}
