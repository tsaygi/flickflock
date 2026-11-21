// A soft-edged wipe from left to right.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let soft = 0.06;
    let edge = p * (1.0 + soft * 2.0) - soft;
    let m = 1.0 - smoothstep(edge - soft, edge + soft, uv.x);
    return mix(get_from(uv), get_to(uv), m);
}
