// The new image slides in from the right, over the old one.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let e = ease(p);
    let incoming = get_to(uv + vec2<f32>(1.0 - e, 0.0));
    let outgoing = get_from(uv);
    return incoming + outgoing * (1.0 - incoming.a);
}
