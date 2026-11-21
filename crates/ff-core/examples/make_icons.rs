//! Renders the application icon in every size/format the packages need:
//!
//!     cargo run --release -p ff-core --example make_icons -- assets
//!
//! The icon is drawn procedurally (signed-distance shapes, 4× supersampled),
//! so there is no binary source artwork to keep in sync.

use image::{imageops, ImageFormat, Rgba, RgbaImage};
use std::io::Write;
use std::path::Path;

fn rounded_rect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let (dx, dy) = ((px - cx).abs() - hw + r, (py - cy).abs() - hh + r);
    dx.max(dy).min(0.0) + (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() - r
}

fn rotate(px: f32, py: f32, cx: f32, cy: f32, deg: f32) -> (f32, f32) {
    let (s, c) = deg.to_radians().sin_cos();
    let (x, y) = (px - cx, py - cy);
    (cx + x * c + y * s, cy - x * s + y * c)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Coverage (0..1) of a shape from its signed distance, with ~1 px of anti-aliasing.
fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn over(dst: &mut [f32; 4], color: [f32; 3], alpha: f32) {
    for c in 0..3 {
        dst[c] = color[c] * alpha + dst[c] * (1.0 - alpha);
    }
    dst[3] = alpha + dst[3] * (1.0 - alpha);
}

fn render(size: u32) -> RgbaImage {
    let s = size as f32;
    RgbaImage::from_fn(size, size, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let v = py / s;
        let mut out = [0.0f32; 4];
        // Background: dark rounded square with a subtle vertical gradient.
        let bg = rounded_rect(px, py, s / 2.0, s / 2.0, s * 0.43, s * 0.43, s * 0.2);
        over(
            &mut out,
            mix([0.13, 0.14, 0.19], [0.06, 0.06, 0.09], v),
            cover(bg),
        );
        // Three fanned photo cards: a small flock, flicked from left to right.
        let cards: [(f32, f32); 3] = [(-0.115, -13.0), (0.0, 0.0), (0.115, 13.0)];
        for (i, (dx, deg)) in cards.iter().enumerate() {
            let (cx, cy) = (s * (0.5 + dx), s * (0.5 + dx.abs() * 0.22));
            let (rx, ry) = rotate(px, py, cx, cy + s * 0.25, *deg);
            let (hw, hh) = (s * 0.17, s * 0.215);
            let d = rounded_rect(rx, ry, cx, cy, hw, hh, s * 0.035);
            // Soft drop shadow under each card.
            let shadow = rounded_rect(rx, ry - s * 0.012, cx, cy, hw, hh, s * 0.035);
            over(
                &mut out,
                [0.0, 0.0, 0.0],
                0.35 * (1.0 - (shadow / (s * 0.03)).clamp(0.0, 1.0)) * cover(bg),
            );
            // White frame, then the "photo" inside it.
            over(&mut out, [0.97, 0.97, 0.98], cover(d));
            let inner = rounded_rect(
                rx,
                ry,
                cx,
                cy - s * 0.012,
                hw - s * 0.022,
                hh - s * 0.036,
                s * 0.015,
            );
            // Photo-local coordinates: x and y in -1..1, `unit` pixels per unit.
            let unit = hw - s * 0.022;
            let (lx, ly) = ((rx - cx) / unit, (ry - (cy - s * 0.012)) / unit);
            let photo = scene(i, lx, ly, unit);
            over(&mut out, photo, cover(inner));
        }
        Rgba([
            (out[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (out[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (out[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (out[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        ])
    })
}

fn circle(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

/// Distance to the line segment a–b.
fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py, bx, by) = (x - a.0, y - a.1, b.0 - a.0, b.1 - a.1);
    let h = ((px * bx + py * by) / (bx * bx + by * by)).clamp(0.0, 1.0);
    ((px - bx * h).powi(2) + (py - by * h).powi(2)).sqrt()
}

/// A rolling hill: negative below the ridge line.
fn hill(x: f32, y: f32, base: f32, amp: f32, freq: f32, phase: f32) -> f32 {
    (base - amp * (x * freq + phase).cos()) - y
}

/// A triangular mountain peak: negative inside.
fn peak(x: f32, y: f32, px: f32, top: f32, slope: f32) -> f32 {
    (top + (x - px).abs() * slope) - y
}

/// The little picture on each card. `x`/`y` are photo-local (-1..1 across
/// the width, y down), `unit` converts those units to pixels for anti-aliasing.
fn scene(card: usize, x: f32, y: f32, unit: f32) -> [f32; 3] {
    let aa = |d: f32| cover(d * unit);
    let mut c;
    match card {
        // Night: crescent moon, stars and snow-capped mountains.
        0 => {
            c = mix(
                [0.10, 0.13, 0.36],
                [0.30, 0.34, 0.74],
                ((y + 1.3) / 2.6).clamp(0.0, 1.0),
            );
            for (sx, sy, r) in [
                (-0.6, -0.9, 0.035),
                (-0.15, -0.55, 0.028),
                (0.1, -1.0, 0.03),
                (0.65, -0.35, 0.03),
                (-0.7, -0.3, 0.025),
            ] {
                c = mix(c, [1.0, 1.0, 0.92], aa(circle(x, y, sx, sy, r)));
            }
            let moon = circle(x, y, 0.42, -0.78, 0.2).max(-circle(x, y, 0.52, -0.84, 0.17));
            c = mix(c, [1.0, 0.96, 0.78], aa(moon));
            let far = peak(x, y, 0.45, 0.05, 1.25);
            c = mix(c, [0.20, 0.22, 0.52], aa(far));
            let near = peak(x, y, -0.3, -0.2, 1.35);
            c = mix(c, [0.13, 0.14, 0.36], aa(near));
            // Snow cap with a jagged lower edge.
            let cap = near.max(y - (0.02 + 0.07 * (x * 22.0).sin()));
            c = mix(c, [0.93, 0.95, 1.0], aa(cap));
        }
        // Sunset: a big low sun over layered hills.
        1 => {
            c = mix(
                [0.99, 0.80, 0.36],
                [0.96, 0.42, 0.34],
                ((y + 1.3) / 1.6).clamp(0.0, 1.0),
            );
            c = mix(
                c,
                [1.0, 0.93, 0.62],
                0.35 * aa(circle(x, y, 0.1, 0.05, 0.52)),
            );
            c = mix(c, [1.0, 0.97, 0.80], aa(circle(x, y, 0.1, 0.05, 0.36)));
            c = mix(c, [0.80, 0.27, 0.36], aa(hill(x, y, 0.42, 0.16, 2.6, 0.9)));
            c = mix(c, [0.52, 0.15, 0.32], aa(hill(x, y, 0.72, 0.2, 2.1, 3.6)));
            c = mix(c, [0.33, 0.09, 0.25], aa(hill(x, y, 1.02, 0.14, 3.0, 1.2)));
        }
        // Day: sun, a cloud, green hills and a small flock of birds.
        _ => {
            c = mix(
                [0.36, 0.72, 0.96],
                [0.74, 0.93, 0.96],
                ((y + 1.3) / 1.7).clamp(0.0, 1.0),
            );
            c = mix(c, [1.0, 0.90, 0.42], aa(circle(x, y, 0.5, -0.82, 0.2)));
            let cloud = circle(x, y, -0.42, -0.42, 0.17)
                .min(circle(x, y, -0.2, -0.5, 0.21))
                .min(circle(x, y, 0.03, -0.42, 0.15))
                .min(segment(x, y, (-0.42, -0.34), (0.03, -0.34)) - 0.09);
            c = mix(c, [1.0, 1.0, 1.0], aa(cloud));
            for (bx, by, w) in [
                (-0.5, -0.98, 0.13),
                (-0.18, -0.86, 0.1),
                (0.08, -1.02, 0.085),
            ] {
                let wing = segment(x, y, (bx - w, by - w * 0.55), (bx, by)).min(segment(
                    x,
                    y,
                    (bx, by),
                    (bx + w, by - w * 0.55),
                ));
                c = mix(c, [0.16, 0.22, 0.32], aa(wing - 0.022));
            }
            c = mix(c, [0.45, 0.80, 0.52], aa(hill(x, y, 0.35, 0.17, 2.4, 2.6)));
            c = mix(c, [0.20, 0.62, 0.45], aa(hill(x, y, 0.68, 0.2, 2.2, 0.4)));
            c = mix(c, [0.10, 0.44, 0.38], aa(hill(x, y, 1.0, 0.13, 3.2, 4.2)));
        }
    }
    c
}

fn png_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encode png");
    bytes
}

/// Minimal ICNS writer: PNG-compressed entries for the modern icon types.
fn write_icns(path: &Path, master: &RgbaImage) {
    let entries: [(&[u8; 4], u32); 7] = [
        (b"icp4", 16),
        (b"icp5", 32),
        (b"icp6", 64),
        (b"ic07", 128),
        (b"ic08", 256),
        (b"ic09", 512),
        (b"ic10", 1024),
    ];
    let mut body = Vec::new();
    for (tag, size) in entries {
        let png = png_bytes(&imageops::resize(
            master,
            size,
            size,
            imageops::FilterType::Lanczos3,
        ));
        body.extend_from_slice(tag);
        body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(&png);
    }
    let mut file = std::fs::File::create(path).expect("create icns");
    file.write_all(b"icns").expect("write");
    file.write_all(&(body.len() as u32 + 8).to_be_bytes())
        .expect("write");
    file.write_all(&body).expect("write");
}

/// ICO with PNG-compressed entries (supported since Windows Vista).
fn write_ico(path: &Path, master: &RgbaImage) {
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let pngs: Vec<Vec<u8>> = sizes
        .iter()
        .map(|s| {
            png_bytes(&imageops::resize(
                master,
                *s,
                *s,
                imageops::FilterType::Lanczos3,
            ))
        })
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (size, png) in sizes.iter().zip(&pngs) {
        let dim = if *size == 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &pngs {
        out.extend_from_slice(png);
    }
    std::fs::write(path, out).expect("write ico");
}

fn main() {
    let out = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "assets".into());
    std::fs::create_dir_all(&out).expect("mkdir");
    // Supersample for clean edges.
    let master = imageops::resize(&render(2048), 1024, 1024, imageops::FilterType::Lanczos3);
    for size in [32u32, 64, 128, 256, 512] {
        imageops::resize(&master, size, size, imageops::FilterType::Lanczos3)
            .save(out.join(format!("icon-{size}.png")))
            .expect("save png");
    }
    master.save(out.join("icon-1024.png")).expect("save png");
    write_icns(&out.join("icon.icns"), &master);
    write_ico(&out.join("icon.ico"), &master);
    println!("icons written to {}", out.display());
}
