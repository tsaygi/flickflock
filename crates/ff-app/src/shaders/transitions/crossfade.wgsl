fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    return mix(get_from(uv), get_to(uv), ease(p));
}
