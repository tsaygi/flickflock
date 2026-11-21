//! Real decodes through ImageIO. Test images are generated on the fly: a hand-written PNG/TIFF
//! encoder produces the source files and the system `sips` tool converts them to HEIC.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::decoder;

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const WHITE: [u8; 3] = [255, 255, 255];

/// A scratch directory that is removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ff-platform-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Four quadrants: red top-left, green top-right, blue bottom-left, white bottom-right.
fn quadrants(width: usize, height: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let color = match (x < width / 2, y < height / 2) {
                (true, true) => RED,
                (false, true) => GREEN,
                (true, false) => BLUE,
                (false, false) => WHITE,
            };
            rgba.extend_from_slice(&color);
            rgba.push(255);
        }
    }
    rgba
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in chunks.iter().copied().flatten() {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Writes an RGBA8 PNG using stored (uncompressed) deflate blocks.
fn write_png(path: &Path, width: usize, height: usize, rgba: &[u8]) {
    assert_eq!(rgba.len(), width * height * 4);
    let mut raw = Vec::new();
    for row in rgba.chunks_exact(width * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(0xFFFF).peekable();
    while let Some(block) = blocks.next() {
        zlib.push(u8::from(blocks.peek().is_none()));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (b"IHDR", &ihdr[..]),
        (b"IDAT", &zlib[..]),
        (b"IEND", &[][..]),
    ] {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        png.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    }
    std::fs::write(path, png).unwrap();
}

/// Writes an uncompressed RGB8 TIFF carrying the given EXIF-style orientation tag.
fn write_tiff(path: &Path, width: usize, height: usize, rgba: &[u8], orientation: u16) {
    const ENTRIES: u16 = 10;
    let bits_offset = 8 + 2 + u32::from(ENTRIES) * 12 + 4;
    let pixels_offset = bits_offset + 6;
    let rgb: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();

    let mut tiff = b"II*\0".to_vec();
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&ENTRIES.to_le_bytes());
    let mut entry = |tag: u16, kind: u16, count: u32, value: u32| {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&kind.to_le_bytes());
        tiff.extend_from_slice(&count.to_le_bytes());
        tiff.extend_from_slice(&value.to_le_bytes());
    };
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    entry(256, LONG, 1, width as u32); // ImageWidth
    entry(257, LONG, 1, height as u32); // ImageLength
    entry(258, SHORT, 3, bits_offset); // BitsPerSample -> [8, 8, 8]
    entry(259, SHORT, 1, 1); // Compression: none
    entry(262, SHORT, 1, 2); // PhotometricInterpretation: RGB
    entry(273, LONG, 1, pixels_offset); // StripOffsets
    entry(274, SHORT, 1, u32::from(orientation)); // Orientation
    entry(277, SHORT, 1, 3); // SamplesPerPixel
    entry(278, LONG, 1, height as u32); // RowsPerStrip
    entry(279, LONG, 1, rgb.len() as u32); // StripByteCounts
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no further IFDs
    for _ in 0..3 {
        tiff.extend_from_slice(&8u16.to_le_bytes());
    }
    assert_eq!(tiff.len(), pixels_offset as usize);
    tiff.extend_from_slice(&rgb);
    std::fs::write(path, tiff).unwrap();
}

/// Converts `input` to HEIC with the system `sips` tool.
fn to_heic(input: &Path, output: &Path) {
    let result = Command::new("/usr/bin/sips")
        .args(["-s", "format", "heic"])
        .arg(input)
        .arg("--out")
        .arg(output)
        .output()
        .expect("sips should be runnable");
    assert!(
        result.status.success() && output.exists(),
        "sips failed: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn pixel(image: &RawImage, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * image.width as usize + x as usize) * 4;
    image.rgba[i..i + 4].try_into().unwrap()
}

#[track_caller]
fn assert_near(actual: [u8; 4], expected: [u8; 3], tolerance: u8) {
    for (a, e) in actual.iter().zip(expected) {
        assert!(
            a.abs_diff(e) <= tolerance,
            "pixel {actual:?} is not close to {expected:?}"
        );
    }
    assert_eq!(actual[3], 255, "pixel {actual:?} should be opaque");
}

/// Checks the centre of each quadrant, given the colours expected at
/// [top-left, top-right, bottom-left, bottom-right].
#[track_caller]
fn assert_quadrants(image: &RawImage, expected: [[u8; 3]; 4], tolerance: u8) {
    assert_eq!(
        image.rgba.len(),
        image.width as usize * image.height as usize * 4
    );
    let (w, h) = (image.width, image.height);
    assert_near(pixel(image, w / 4, h / 4), expected[0], tolerance);
    assert_near(pixel(image, 3 * w / 4, h / 4), expected[1], tolerance);
    assert_near(pixel(image, w / 4, 3 * h / 4), expected[2], tolerance);
    assert_near(pixel(image, 3 * w / 4, 3 * h / 4), expected[3], tolerance);
}

const UPRIGHT: [[u8; 3]; 4] = [RED, GREEN, BLUE, WHITE];
/// The quadrant image after a 90 degree clockwise rotation (orientation tag 6).
const ROTATED_CW: [[u8; 3]; 4] = [BLUE, RED, WHITE, GREEN];
/// Lossy HEVC plus chroma subsampling shifts saturated colours a little.
const HEIC_TOLERANCE: u8 = 16;

#[test]
fn png_decodes_exactly() {
    let dir = TempDir::new("png");
    let png = dir.file("q.png");
    write_png(&png, 64, 48, &quadrants(64, 48));

    let image = decoder().decode(&png).unwrap();
    assert_eq!((image.width, image.height), (64, 48));
    assert_quadrants(&image, UPRIGHT, 0);
    // Row order: the very first pixel is the top-left one.
    assert_eq!(pixel(&image, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&image, 63, 47), [255, 255, 255, 255]);
}

#[test]
fn heic_decodes_at_full_size() {
    let dir = TempDir::new("heic-full");
    let (png, heic) = (dir.file("q.png"), dir.file("q.heic"));
    write_png(&png, 200, 150, &quadrants(200, 150));
    to_heic(&png, &heic);

    let image = decoder().decode(&heic).unwrap();
    assert_eq!((image.width, image.height), (200, 150));
    assert_quadrants(&image, UPRIGHT, HEIC_TOLERANCE);
}

#[test]
fn heic_decodes_scaled() {
    let dir = TempDir::new("heic-scaled");
    let (png, heic) = (dir.file("q.png"), dir.file("q.heic"));
    write_png(&png, 200, 150, &quadrants(200, 150));
    to_heic(&png, &heic);

    let small = decoder().decode_scaled(&heic, 100).unwrap();
    // ImageIO may round the short side of a HEVC thumbnail up to an even number.
    assert_eq!(small.width, 100);
    assert!(
        (75..=76).contains(&small.height),
        "unexpected height {}",
        small.height
    );
    assert_quadrants(&small, UPRIGHT, HEIC_TOLERANCE);

    // Never upscaled: asking for more than the native size returns the native size.
    let native = decoder().decode_scaled(&heic, 4096).unwrap();
    assert_eq!((native.width, native.height), (200, 150));
    assert_quadrants(&native, UPRIGHT, HEIC_TOLERANCE);

    // A zero limit means "no limit".
    let unlimited = decoder().decode_scaled(&heic, 0).unwrap();
    assert_eq!((unlimited.width, unlimited.height), (200, 150));
}

#[test]
fn orientation_tag_is_applied() {
    let dir = TempDir::new("orientation");
    let tiff = dir.file("rotated.tiff");
    // Stored 64x48 with orientation 6: must be shown rotated 90 degrees clockwise, 48x64.
    write_tiff(&tiff, 64, 48, &quadrants(64, 48), 6);

    let image = decoder().decode(&tiff).unwrap();
    assert_eq!((image.width, image.height), (48, 64));
    assert_quadrants(&image, ROTATED_CW, 0);

    let small = decoder().decode_scaled(&tiff, 32).unwrap();
    assert_eq!((small.width, small.height), (24, 32));
    assert_quadrants(&small, ROTATED_CW, 2);

    // Orientation 3 (upside down) keeps the size and swaps the quadrants diagonally.
    let flipped = dir.file("flipped.tiff");
    write_tiff(&flipped, 64, 48, &quadrants(64, 48), 3);
    let image = decoder().decode(&flipped).unwrap();
    assert_eq!((image.width, image.height), (64, 48));
    assert_quadrants(&image, [WHITE, BLUE, GREEN, RED], 0);
}

#[test]
fn heic_made_from_rotated_source_is_upright() {
    let dir = TempDir::new("heic-orientation");
    let (tiff, heic) = (dir.file("rotated.tiff"), dir.file("rotated.heic"));
    write_tiff(&tiff, 200, 150, &quadrants(200, 150), 6);
    to_heic(&tiff, &heic);

    let image = decoder().decode(&heic).unwrap();
    assert_eq!((image.width, image.height), (150, 200));
    assert_quadrants(&image, ROTATED_CW, HEIC_TOLERANCE);

    let small = decoder().decode_scaled(&heic, 100).unwrap();
    assert_eq!(small.height, 100);
    assert!(
        (75..=76).contains(&small.width),
        "unexpected width {}",
        small.width
    );
    assert_quadrants(&small, ROTATED_CW, HEIC_TOLERANCE);
}

#[test]
fn alpha_is_returned_straight() {
    let dir = TempDir::new("alpha");
    let png = dir.file("alpha.png");
    let mut rgba = Vec::new();
    for i in 0..16 * 16 {
        rgba.extend_from_slice(match i % 3 {
            0 => &[200, 100, 50, 128],
            1 => &[10, 20, 30, 0],
            _ => &[1, 2, 3, 255],
        });
    }
    write_png(&png, 16, 16, &rgba);

    let image = decoder().decode(&png).unwrap();
    assert_eq!((image.width, image.height), (16, 16));
    let translucent = pixel(&image, 0, 0);
    assert_eq!(translucent[3], 128);
    for (actual, expected) in translucent.iter().zip([200u8, 100, 50]) {
        assert!(
            actual.abs_diff(expected) <= 2,
            "{translucent:?} lost its straight colour"
        );
    }
    assert_eq!(pixel(&image, 1, 0)[3], 0);
    assert_eq!(pixel(&image, 2, 0), [1, 2, 3, 255]);
}

#[test]
fn unpremultiply_restores_straight_alpha() {
    let mut pixels = [100, 50, 25, 128, 9, 9, 9, 0, 7, 8, 9, 255, 255, 255, 255, 1];
    unpremultiply(&mut pixels);
    assert_eq!(&pixels[..4], &[199, 100, 50, 128]);
    assert_eq!(&pixels[4..8], &[9, 9, 9, 0]);
    assert_eq!(&pixels[8..12], &[7, 8, 9, 255]);
    // Invalid premultiplied input (colour > alpha) saturates instead of wrapping.
    assert_eq!(&pixels[12..], &[255, 255, 255, 1]);
}

#[test]
fn directory_is_an_error() {
    let dir = TempDir::new("dir");
    assert!(decoder().decode(&dir.0).is_err());
}
