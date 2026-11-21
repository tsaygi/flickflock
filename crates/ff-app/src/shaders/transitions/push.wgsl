// The new image pushes the old one out to the left.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let e = ease(p);
    return get_from(uv + vec2<f32>(e, 0.0)) + get_to(uv - vec2<f32>(1.0 - e, 0.0));
}
