// The old image zooms in and fades while the new one settles from slightly smaller.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let e = ease(p);
    let c = vec2<f32>(0.5);
    let outgoing = get_from((uv - c) / (1.0 + e * 0.6) + c);
    let incoming = get_to((uv - c) / (0.85 + 0.15 * e) + c);
    return mix(outgoing, incoming, e);
}
