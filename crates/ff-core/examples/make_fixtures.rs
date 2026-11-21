//! Generates the test fixtures used for manual testing and benchmarks:
//!
//!     cargo run --release -p ff-core --example make_fixtures -- tests/fixtures/out
//!
//! Nothing here is committed; everything is synthesised.

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, DynamicImage, Frame, ImageFormat, Rgba, RgbaImage};
use img_parts::{Bytes, ImageEXIF};
use std::fs;
use std::path::Path;

/// A colourful test card with a number of big "digit bars" so images are easy to tell apart.
fn card(w: u32, h: u32, seed: u32) -> RgbaImage {
    let hue = (seed * 47) % 360;
    RgbaImage::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let (r, g, b) = hsv(
            (hue as f32 + fx * 60.0) % 360.0,
            0.55 + 0.3 * fy,
            0.95 - 0.5 * fy,
        );
        // Fine checker in the corner to judge sharpness; bars encode the seed in binary.
        let checker = x < w / 8 && y < h / 8 && (x / 2 + y / 2) % 2 == 0;
        let bar = (seed >> (((fx * 8.0) as u32).min(7))) & 1 == 1 && (0.42..0.58).contains(&fy);
        let grid = x % 100 == 0 || y % 100 == 0;
        if checker || bar {
            Rgba([255, 255, 255, 255])
        } else if grid {
            Rgba([r / 2, g / 2, b / 2, 255])
        } else {
            Rgba([r, g, b, 255])
        }
    })
}

fn hsv(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

fn save_jpeg(img: &RgbaImage, path: &Path, exif: Option<Vec<u8>>) {
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(img.clone())
        .into_rgb8()
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Jpeg)
        .expect("encode jpeg");
    if let Some(exif) = exif {
        let mut jpeg = img_parts::jpeg::Jpeg::from_bytes(Bytes::from(bytes)).expect("parse jpeg");
        jpeg.set_exif(Some(Bytes::from(exif)));
        bytes = Vec::new();
        jpeg.encoder().write_to(&mut bytes).expect("write jpeg");
    }
    fs::write(path, bytes).expect("write file");
}

fn animated_gif(path: &Path, frames: u32, size: u32) {
    let mut enc = GifEncoder::new_with_speed(fs::File::create(path).expect("create gif"), 30);
    enc.set_repeat(Repeat::Infinite).expect("repeat");
    for i in 0..frames {
        let t = i as f32 / frames as f32;
        let img = RgbaImage::from_fn(size, size, |x, y| {
            // A dot orbiting the centre plus a progress bar: direction and frame position are obvious.
            let cx = size as f32 / 2.0 + (t * std::f32::consts::TAU).cos() * size as f32 * 0.32;
            let cy = size as f32 / 2.0 + (t * std::f32::consts::TAU).sin() * size as f32 * 0.32;
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d < size as f32 * 0.07 {
                Rgba([255, 200, 40, 255])
            } else if y > size - 8 && (x as f32) < t * size as f32 {
                Rgba([80, 200, 255, 255])
            } else {
                Rgba([24, 28, 44, 255])
            }
        });
        enc.encode_frame(Frame::from_parts(
            img,
            0,
            0,
            Delay::from_numer_denom_ms(40, 1),
        ))
        .expect("frame");
    }
}

fn main() {
    let out = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "tests/fixtures/out".into());
    let _ = fs::remove_dir_all(&out);

    // 1. A nested trip with an empty day (folder navigation acceptance test).
    for (dir, count) in [
        ("Trip/Day1", 10),
        ("Trip/Day2", 0),
        ("Trip/Day3", 5),
        ("Trip/Day10", 3),
        ("Trip/Day3/Evening", 2),
    ] {
        fs::create_dir_all(out.join(dir)).expect("mkdir");
        for i in 1..=count {
            let seed = dir.len() as u32 * 7 + i;
            save_jpeg(
                &card(1600, 1067, seed),
                &out.join(dir).join(format!("photo{i}.jpg")),
                None,
            );
        }
    }
    fs::create_dir_all(out.join("Trip/.hidden")).expect("mkdir");
    save_jpeg(
        &card(200, 200, 1),
        &out.join("Trip/.hidden/secret.jpg"),
        None,
    );

    // 2. One file per format, plus awkward names.
    let formats = out.join("Formats");
    fs::create_dir_all(&formats).expect("mkdir");
    let base = card(1200, 800, 3);
    for (name, fmt) in [
        ("image.png", ImageFormat::Png),
        ("image.bmp", ImageFormat::Bmp),
        ("image.tiff", ImageFormat::Tiff),
        ("image.tga", ImageFormat::Tga),
        ("image.qoi", ImageFormat::Qoi),
        ("image.webp", ImageFormat::WebP),
        ("image.gif", ImageFormat::Gif),
        ("image.ico", ImageFormat::Ico),
    ] {
        let img = if fmt == ImageFormat::Ico {
            image::imageops::thumbnail(&base, 256, 171)
        } else {
            base.clone()
        };
        img.save_with_format(formats.join(name), fmt).expect(name);
    }
    DynamicImage::ImageRgba8(base.clone())
        .into_rgb8()
        .save(formats.join("image.ppm"))
        .expect("ppm");
    DynamicImage::ImageRgba8(base.clone())
        .into_rgb32f()
        .save(formats.join("image.exr"))
        .expect("exr");
    DynamicImage::ImageRgba8(base.clone())
        .into_rgb32f()
        .save(formats.join("image.hdr"))
        .expect("hdr");
    DynamicImage::ImageRgba8(base.clone())
        .into_rgba16()
        .save(formats.join("image.ff"))
        .expect("farbfeld");
    save_jpeg(&base, &formats.join("image.jpg"), None);
    save_jpeg(&base, &formats.join("写真 その1.jpg"), None);
    save_jpeg(&base, &formats.join("with spaces & ümlauts.jpg"), None);
    save_jpeg(&base, &formats.join("img2.jpg"), None);
    save_jpeg(&base, &formats.join("img10.jpg"), None);
    base.save_with_format(formats.join("png_named_as.jpg"), ImageFormat::Png)
        .expect("misnamed");
    fs::write(
        formats.join("broken.png"),
        b"\x89PNG\r\n\x1a\n this is not really a png",
    )
    .expect("broken");
    fs::write(formats.join("empty.jpg"), b"").expect("empty");
    // Transparency.
    RgbaImage::from_fn(600, 400, |x, y| {
        let d =
            (((x as f32 - 300.0) / 300.0).powi(2) + ((y as f32 - 200.0) / 200.0).powi(2)).sqrt();
        Rgba([240, 90, 60, (255.0 * (1.0 - d).clamp(0.0, 1.0)) as u8])
    })
    .save(formats.join("transparent.png"))
    .expect("transparent");
    // Pixel art (tiny).
    RgbaImage::from_fn(16, 16, |x, y| {
        if (x + y) % 2 == 0 {
            Rgba([20, 20, 20, 255])
        } else {
            Rgba([250, 220, 60, 255])
        }
    })
    .save(formats.join("pixel-art.png"))
    .expect("pixel art");

    // 3. EXIF orientations 1–8 (all should display upright: white checker top-left).
    let exif_dir = out.join("Orientation");
    fs::create_dir_all(&exif_dir).expect("mkdir");
    let upright = card(900, 600, 5);
    for o in 1..=8u8 {
        // Store pixels pre-transformed with the inverse so the tag restores the upright image.
        let mut stored = DynamicImage::ImageRgba8(upright.clone());
        let inverse = match o {
            1 => image::metadata::Orientation::NoTransforms,
            2 => image::metadata::Orientation::FlipHorizontal,
            3 => image::metadata::Orientation::Rotate180,
            4 => image::metadata::Orientation::FlipVertical,
            5 => image::metadata::Orientation::Rotate90FlipH,
            6 => image::metadata::Orientation::Rotate270,
            7 => image::metadata::Orientation::Rotate270FlipH,
            _ => image::metadata::Orientation::Rotate90,
        };
        stored.apply_orientation(inverse);
        save_jpeg(
            &stored.into_rgba8(),
            &exif_dir.join(format!("orientation{o}.jpg")),
            Some(ff_core::meta::minimal_exif(u16::from(o))),
        );
    }
    save_jpeg(&upright, &exif_dir.join("no-exif.jpg"), None);

    // 4. Animations.
    let anim = out.join("Animated");
    fs::create_dir_all(&anim).expect("mkdir");
    animated_gif(&anim.join("orbit-60.gif"), 60, 240);
    animated_gif(&anim.join("orbit-500.gif"), 500, 200);

    // 5. Huge images: a 20,000 px wide panorama and a 50 MP photo.
    let huge = out.join("Huge");
    fs::create_dir_all(&huge).expect("mkdir");
    save_jpeg(
        &card(20_000, 3000, 9),
        &huge.join("panorama-20000.jpg"),
        None,
    );
    save_jpeg(&card(8660, 5773, 11), &huge.join("photo-50mp.jpg"), None);

    // 6. A big folder for index/strip performance.
    let many = out.join("Many");
    fs::create_dir_all(&many).expect("mkdir");
    let small: Vec<Vec<u8>> = (0..16)
        .map(|i| {
            let mut bytes = Vec::new();
            DynamicImage::ImageRgba8(card(640, 427, i))
                .into_rgb8()
                .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Jpeg)
                .expect("encode");
            bytes
        })
        .collect();
    for i in 1..=2000 {
        fs::write(many.join(format!("IMG_{i}.jpg")), &small[i % 16]).expect("write");
    }
    println!("fixtures written to {}", out.display());
}
