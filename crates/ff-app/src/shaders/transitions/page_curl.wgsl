// A simplified page curl: the old page rolls away towards the left around a
// vertical cylinder, revealing the new image underneath.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let radius = 0.12;
    let pi = 3.14159265;
    // x position of the cylinder axis, travelling from right of the page to left of it.
    let axis = 1.0 + radius - p * (1.0 + 2.0 * radius + pi * radius);
    let incoming = get_to(uv);
    let dx = uv.x - axis;
    if dx < 0.0 {
        // Still-flat part of the old page.
        return get_from(uv);
    }
    if dx > radius {
        // Past the curl: only the new image (with a soft shadow next to the roll).
        let shadow = 1.0 - 0.35 * (1.0 - smoothstep(0.0, 0.12, dx - radius)) * (1.0 - p);
        return vec4<f32>(incoming.rgb * shadow, incoming.a);
    }
    // On the cylinder. The same screen x shows the front (lower) and back (upper) of the roll.
    let theta = asin(dx / radius);
    let front_x = axis + theta * radius;
    let back_x = axis + (pi - theta) * radius;
    var color = incoming;
    let front = get_from(vec2<f32>(front_x, uv.y));
    color = front + color * (1.0 - front.a);
    if back_x <= 1.0 {
        // Back of the page: a lightened, mirrored copy.
        let back = get_from(vec2<f32>(back_x, uv.y));
        let shade = 0.55 + 0.45 * cos(theta);
        let paper = vec4<f32>(mix(back.rgb, vec3<f32>(back.a), 0.6) * shade, back.a);
        color = paper + color * (1.0 - paper.a);
    }
    return color;
}
