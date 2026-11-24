//! Basic, non-destructive editing: rotate, flip, crop and canvas extension
//! with undo/redo, plus saving (with EXIF preservation and lossless JPEG
//! rotation through the EXIF orientation tag).

use image::metadata::Orientation;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use img_parts::{Bytes, DynImage, ImageEXIF};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOp {
    RotateLeft,
    RotateRight,
    FlipHorizontal,
    FlipVertical,
    /// Keep the rectangle `x, y, width, height` (in current image pixels).
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    /// Add borders of the given widths filled with `color` (straight RGBA).
    Extend {
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
        color: [u8; 4],
    },
}

impl EditOp {
    fn is_orientation_only(self) -> bool {
        matches!(
            self,
            Self::RotateLeft | Self::RotateRight | Self::FlipHorizontal | Self::FlipVertical
        )
    }

    pub fn apply(self, img: &RgbaImage) -> RgbaImage {
        use image::imageops;
        match self {
            Self::RotateLeft => imageops::rotate270(img),
            Self::RotateRight => imageops::rotate90(img),
            Self::FlipHorizontal => imageops::flip_horizontal(img),
            Self::FlipVertical => imageops::flip_vertical(img),
            Self::Crop {
                x,
                y,
                width,
                height,
            } => {
                let x = x.min(img.width().saturating_sub(1));
                let y = y.min(img.height().saturating_sub(1));
                let w = width.clamp(1, (img.width() - x).max(1));
                let h = height.clamp(1, (img.height() - y).max(1));
                imageops::crop_imm(img, x, y, w, h).to_image()
            }
            Self::Extend {
                left,
                top,
                right,
                bottom,
                color,
            } => {
                // Keep the result within sane bounds.
                const MAX_EDGE: u32 = 65_535;
                let w = img
                    .width()
                    .saturating_add(left)
                    .saturating_add(right)
                    .min(MAX_EDGE);
                let h = img
                    .height()
                    .saturating_add(top)
                    .saturating_add(bottom)
                    .min(MAX_EDGE);
                let mut out = RgbaImage::from_pixel(w, h, Rgba(color));
                imageops::replace(&mut out, img, i64::from(left), i64::from(top));
                out
            }
        }
    }
}

/// Common crop aspect ratios (width : height). `None` = free.
pub const ASPECT_PRESETS: [(&str, Option<(u32, u32)>); 8] = [
    ("Free", None),
    ("1:1", Some((1, 1))),
    ("4:3", Some((4, 3))),
    ("3:2", Some((3, 2))),
    ("16:9", Some((16, 9))),
    ("3:4", Some((3, 4))),
    ("2:3", Some((2, 3))),
    ("9:16", Some((9, 16))),
];

/// An editing session over one image. The original pixels are kept, so every
/// edit can be undone until the result is saved.
#[derive(Debug, Clone)]
pub struct EditSession {
    original: Arc<RgbaImage>,
    current: Arc<RgbaImage>,
    done: Vec<EditOp>,
    undone: Vec<EditOp>,
}

impl EditSession {
    pub fn new(original: RgbaImage) -> Self {
        let original = Arc::new(original);
        Self {
            current: original.clone(),
            original,
            done: Vec::new(),
            undone: Vec::new(),
        }
    }

    pub fn current(&self) -> &Arc<RgbaImage> {
        &self.current
    }

    pub fn ops(&self) -> &[EditOp] {
        &self.done
    }

    pub fn is_dirty(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn apply(&mut self, op: EditOp) {
        self.current = Arc::new(op.apply(&self.current));
        self.done.push(op);
        self.undone.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(op) = self.done.pop() else {
            return false;
        };
        self.undone.push(op);
        self.rebuild();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(op) = self.undone.pop() else {
            return false;
        };
        self.current = Arc::new(op.apply(&self.current));
        self.done.push(op);
        true
    }

    fn rebuild(&mut self) {
        let mut img = self.original.clone();
        for op in &self.done {
            img = Arc::new(op.apply(&img));
        }
        self.current = img;
    }

    /// `true` when the edits are only rotations/flips, which a JPEG can
    /// store losslessly in its EXIF orientation tag.
    pub fn is_orientation_only(&self) -> bool {
        self.is_dirty() && self.done.iter().all(|op| op.is_orientation_only())
    }
}

const ALL_ORIENTATIONS: [Orientation; 8] = [
    Orientation::NoTransforms,
    Orientation::FlipHorizontal,
    Orientation::Rotate180,
    Orientation::FlipVertical,
    Orientation::Rotate90FlipH,
    Orientation::Rotate90,
    Orientation::Rotate270FlipH,
    Orientation::Rotate270,
];

/// The EXIF orientation equivalent to "apply `start`, then `ops`".
///
/// Worked out by brute force on a tiny asymmetric probe image, which keeps
/// this obviously correct without hand-written group tables.
pub fn compose_orientation(start: Orientation, ops: &[EditOp]) -> Option<Orientation> {
    let probe = RgbaImage::from_fn(3, 2, |x, y| Rgba([(x + y * 3) as u8, 0, 0, 255]));
    let mut dynimg = DynamicImage::ImageRgba8(probe.clone());
    dynimg.apply_orientation(start);
    let mut target = dynimg.into_rgba8();
    for op in ops {
        if !op.is_orientation_only() {
            return None;
        }
        target = op.apply(&target);
    }
    ALL_ORIENTATIONS.into_iter().find(|o| {
        let mut candidate = DynamicImage::ImageRgba8(probe.clone());
        candidate.apply_orientation(*o);
        candidate.into_rgba8() == target
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Encode(String),
    #[error("cannot save as .{0}")]
    UnsupportedFormat(String),
}

/// Formats offered by "Save As".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFormat {
    Jpeg,
    Png,
    WebP,
    Tiff,
    Bmp,
    Qoi,
    Tga,
}

impl SaveFormat {
    pub const ALL: [SaveFormat; 7] = [
        Self::Jpeg,
        Self::Png,
        Self::WebP,
        Self::Tiff,
        Self::Bmp,
        Self::Qoi,
        Self::Tga,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::WebP => "WebP (lossless)",
            Self::Tiff => "TIFF",
            Self::Bmp => "BMP",
            Self::Qoi => "QOI",
            Self::Tga => "TGA",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::Qoi => "qoi",
            Self::Tga => "tga",
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_string_lossy().to_lowercase();
        Some(match ext.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::WebP,
            "tif" | "tiff" => Self::Tiff,
            "bmp" => Self::Bmp,
            "qoi" => Self::Qoi,
            "tga" => Self::Tga,
            _ => return None,
        })
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Png => ImageFormat::Png,
            Self::WebP => ImageFormat::WebP,
            Self::Tiff => ImageFormat::Tiff,
            Self::Bmp => ImageFormat::Bmp,
            Self::Qoi => ImageFormat::Qoi,
            Self::Tga => ImageFormat::Tga,
        }
    }
}

fn encode(image: &RgbaImage, format: SaveFormat, jpeg_quality: u8) -> Result<Vec<u8>, SaveError> {
    let mut out = Vec::new();
    let err = |e: image::ImageError| SaveError::Encode(e.to_string());
    match format {
        SaveFormat::Jpeg => {
            // JPEG has no alpha: composite over white.
            let mut rgb = image::RgbImage::new(image.width(), image.height());
            for (dst, src) in rgb.pixels_mut().zip(image.pixels()) {
                let a = u32::from(src.0[3]);
                for c in 0..3 {
                    dst.0[c] = ((u32::from(src.0[c]) * a + 255 * (255 - a) + 127) / 255) as u8;
                }
            }
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out,
                jpeg_quality.clamp(1, 100),
            );
            rgb.write_with_encoder(enc).map_err(err)?;
        }
        other => image
            .write_to(&mut Cursor::new(&mut out), other.image_format())
            .map_err(err)?,
    }
    Ok(out)
}

/// Write `bytes` to `dest` atomically (temp file in the same folder, then rename).
fn write_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp_name = dest.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".ff-tmp{}", std::process::id()));
    let tmp = dest.with_file_name(tmp_name);
    match std::fs::write(&tmp, bytes) {
        // A sandboxed Save As grants only `dest`, not a sibling temp file.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return std::fs::write(dest, bytes);
        }
        result => result?,
    }
    std::fs::rename(&tmp, dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Save edited pixels to `dest`. EXIF from `metadata_source` is carried over
/// when the target container supports it (JPEG, PNG, WebP); its orientation
/// is reset because the pixels are saved upright.
pub fn save_image(
    image: &RgbaImage,
    dest: &Path,
    format: SaveFormat,
    jpeg_quality: u8,
    metadata_source: Option<&Path>,
) -> Result<(), SaveError> {
    let mut bytes = encode(image, format, jpeg_quality)?;
    if matches!(
        format,
        SaveFormat::Jpeg | SaveFormat::Png | SaveFormat::WebP
    ) {
        if let Some(exif) = metadata_source.and_then(read_exif_block) {
            if let Ok(Some(mut container)) = DynImage::from_bytes(Bytes::from(bytes.clone())) {
                container.set_exif(Some(Bytes::from(exif)));
                let mut out = Vec::with_capacity(bytes.len() + 4096);
                if container.encoder().write_to(&mut out).is_ok() {
                    bytes = out;
                }
            }
        }
    }
    Ok(write_atomic(dest, &bytes)?)
}

/// The raw EXIF (TIFF) block of a JPEG/PNG/WebP file with orientation reset to 1.
fn read_exif_block(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let container = DynImage::from_bytes(Bytes::from(bytes)).ok()??;
    let mut exif = container.exif()?.to_vec();
    if let Some(at) = find_orientation_value(&exif) {
        write_u16(&mut exif, at, 1);
    }
    Some(exif)
}

fn is_little_endian(tiff: &[u8]) -> Option<bool> {
    match tiff.get(0..2)? {
        b"II" => Some(true),
        b"MM" => Some(false),
        _ => None,
    }
}

/// Byte offset of the orientation value inside a TIFF/EXIF block (IFD0, SHORT).
fn find_orientation_value(tiff: &[u8]) -> Option<usize> {
    let le = is_little_endian(tiff)?;
    let u16_at = |o: usize| {
        tiff.get(o..o + 2).map(|b| {
            if le {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            }
        })
    };
    let u32_at = |o: usize| {
        tiff.get(o..o + 4).map(|b| {
            if le {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            } else {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]])
            }
        })
    };
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count).map(|i| ifd + 2 + i * 12).find_map(|entry| {
        let is_short_orientation =
            u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3 && u32_at(entry + 4)? == 1;
        (is_short_orientation && entry + 10 <= tiff.len()).then_some(entry + 8)
    })
}

fn write_u16(tiff: &mut [u8], at: usize, value: u16) {
    let bytes = if is_little_endian(tiff).unwrap_or(true) {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    tiff[at..at + 2].copy_from_slice(&bytes);
}

/// Rotate/flip a JPEG **losslessly** by rewriting only its EXIF orientation
/// tag: the compressed image data is copied untouched.
///
/// Returns `Ok(false)` when that is not possible (not a JPEG, non-orientation
/// edits, or EXIF data without an orientation tag that cannot be patched in
/// place) so the caller can fall back to re-encoding.
pub fn save_jpeg_orientation(
    source: &Path,
    dest: &Path,
    ops: &[EditOp],
) -> Result<bool, SaveError> {
    let bytes = std::fs::read(source)?;
    if !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(false);
    }
    let Ok(mut jpeg) = img_parts::jpeg::Jpeg::from_bytes(Bytes::from(bytes)) else {
        return Ok(false);
    };
    let new_exif = match jpeg.exif() {
        Some(existing) => {
            let mut exif = existing.to_vec();
            let Some(at) = find_orientation_value(&exif) else {
                return Ok(false);
            };
            let le = is_little_endian(&exif).unwrap_or(true);
            let raw = if le {
                u16::from_le_bytes([exif[at], exif[at + 1]])
            } else {
                u16::from_be_bytes([exif[at], exif[at + 1]])
            };
            let start = u8::try_from(raw)
                .ok()
                .and_then(Orientation::from_exif)
                .unwrap_or(Orientation::NoTransforms);
            let Some(composed) = compose_orientation(start, ops) else {
                return Ok(false);
            };
            write_u16(&mut exif, at, u16::from(composed.to_exif()));
            exif
        }
        None => {
            let Some(composed) = compose_orientation(Orientation::NoTransforms, ops) else {
                return Ok(false);
            };
            crate::meta::minimal_exif(u16::from(composed.to_exif()))
        }
    };
    jpeg.set_exif(Some(Bytes::from(new_exif)));
    let mut out = Vec::new();
    jpeg.encoder().write_to(&mut out)?;
    write_atomic(dest, &out)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{DecodeOptions, Decoders};

    fn probe() -> RgbaImage {
        RgbaImage::from_fn(4, 3, |x, y| Rgba([x as u8 * 10, y as u8 * 10, 0, 255]))
    }

    #[test]
    fn rotations_and_flips() {
        let img = probe();
        let r = EditOp::RotateRight.apply(&img);
        assert_eq!(r.dimensions(), (3, 4));
        // The top-left pixel moves to the top-right corner.
        assert_eq!(r.get_pixel(2, 0), img.get_pixel(0, 0));
        let l = EditOp::RotateLeft.apply(&r);
        assert_eq!(l, img);
        assert_eq!(
            EditOp::FlipHorizontal.apply(&img).get_pixel(3, 0),
            img.get_pixel(0, 0)
        );
        assert_eq!(
            EditOp::FlipVertical.apply(&img).get_pixel(0, 2),
            img.get_pixel(0, 0)
        );
    }

    #[test]
    fn crop_is_clamped() {
        let img = probe();
        let c = EditOp::Crop {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        }
        .apply(&img);
        assert_eq!(c.dimensions(), (2, 2));
        assert_eq!(c.get_pixel(0, 0), img.get_pixel(1, 1));
        let c = EditOp::Crop {
            x: 3,
            y: 2,
            width: 100,
            height: 100,
        }
        .apply(&img);
        assert_eq!(c.dimensions(), (1, 1));
        let c = EditOp::Crop {
            x: 100,
            y: 100,
            width: 0,
            height: 0,
        }
        .apply(&img);
        assert_eq!(c.dimensions(), (1, 1));
    }

    #[test]
    fn extend_adds_coloured_borders() {
        let img = probe();
        let e = EditOp::Extend {
            left: 2,
            top: 1,
            right: 3,
            bottom: 4,
            color: [9, 8, 7, 255],
        }
        .apply(&img);
        assert_eq!(e.dimensions(), (9, 8));
        assert_eq!(e.get_pixel(0, 0).0, [9, 8, 7, 255]);
        assert_eq!(e.get_pixel(2, 1), img.get_pixel(0, 0));
        assert_eq!(e.get_pixel(8, 7).0, [9, 8, 7, 255]);
    }

    #[test]
    fn undo_redo() {
        let mut s = EditSession::new(probe());
        assert!(!s.is_dirty() && !s.can_undo() && !s.undo());
        s.apply(EditOp::RotateRight);
        s.apply(EditOp::Crop {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        });
        assert_eq!(s.current().dimensions(), (2, 2));
        assert!(!s.is_orientation_only());
        assert!(s.undo());
        assert_eq!(s.current().dimensions(), (3, 4));
        assert!(s.is_orientation_only());
        assert!(s.undo());
        assert_eq!(**s.current(), probe());
        assert!(!s.is_dirty());
        assert!(s.redo() && s.redo() && !s.redo());
        assert_eq!(s.current().dimensions(), (2, 2));
        // A new edit clears the redo stack.
        s.undo();
        s.apply(EditOp::FlipVertical);
        assert!(!s.can_redo());
        assert_eq!(s.ops().len(), 2);
    }

    #[test]
    fn orientation_composition() {
        use EditOp::*;
        let o = Orientation::NoTransforms;
        assert_eq!(compose_orientation(o, &[]), Some(Orientation::NoTransforms));
        assert_eq!(
            compose_orientation(o, &[RotateRight]),
            Some(Orientation::Rotate90)
        );
        assert_eq!(
            compose_orientation(o, &[RotateLeft]),
            Some(Orientation::Rotate270)
        );
        assert_eq!(
            compose_orientation(o, &[RotateRight, RotateRight]),
            Some(Orientation::Rotate180)
        );
        assert_eq!(
            compose_orientation(Orientation::Rotate90, &[RotateLeft]),
            Some(Orientation::NoTransforms)
        );
        assert_eq!(
            compose_orientation(o, &[FlipHorizontal]),
            Some(Orientation::FlipHorizontal)
        );
        assert_eq!(
            compose_orientation(o, &[FlipHorizontal, FlipVertical]),
            Some(Orientation::Rotate180)
        );
        assert_eq!(
            compose_orientation(
                o,
                &[Crop {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1
                }]
            ),
            None
        );
        // Every start/op combination lands on a valid orientation.
        for start in ALL_ORIENTATIONS {
            for op in [RotateLeft, RotateRight, FlipHorizontal, FlipVertical] {
                assert!(compose_orientation(start, &[op]).is_some());
            }
        }
    }

    fn write_jpeg(path: &Path, exif: Option<Vec<u8>>) {
        let img = RgbaImage::from_fn(64, 32, |x, _| {
            if x < 32 {
                Rgba([250, 10, 10, 255])
            } else {
                Rgba([10, 10, 250, 255])
            }
        });
        let bytes = encode(&img, SaveFormat::Jpeg, 95).unwrap();
        let mut jpeg = img_parts::jpeg::Jpeg::from_bytes(Bytes::from(bytes)).unwrap();
        jpeg.set_exif(exif.map(Bytes::from));
        let mut out = Vec::new();
        jpeg.encoder().write_to(&mut out).unwrap();
        std::fs::write(path, out).unwrap();
    }

    fn decode(path: &Path) -> RgbaImage {
        Decoders::builtin()
            .decode(path, &DecodeOptions::default())
            .unwrap()
            .image
    }

    fn is_red(p: &Rgba<u8>) -> bool {
        p.0[0] > 200 && p.0[2] < 60
    }

    #[test]
    fn lossless_jpeg_rotation_keeps_image_data_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        for exif in [
            None,
            Some(crate::meta::minimal_exif(1)),
            Some(crate::meta::minimal_exif(6)),
        ] {
            let src = tmp.path().join("src.jpg");
            let dst = tmp.path().join("dst.jpg");
            write_jpeg(&src, exif);
            let mut expected = EditSession::new(decode(&src));
            expected.apply(EditOp::RotateRight);
            assert!(save_jpeg_orientation(&src, &dst, &[EditOp::RotateRight]).unwrap());
            let got = decode(&dst);
            assert_eq!(got.dimensions(), expected.current().dimensions());
            // Decoded pixels are *identical* to rotating the decoded source: no recompression happened.
            assert_eq!(&got, expected.current().as_ref());
            // The entropy-coded data is byte-identical.
            let scan = |p: &Path| {
                let b = std::fs::read(p).unwrap();
                let sos = b.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
                b[sos..].to_vec()
            };
            assert_eq!(scan(&src), scan(&dst));
        }
    }

    #[test]
    fn lossless_rotation_refuses_what_it_cannot_do() {
        let tmp = tempfile::tempdir().unwrap();
        let png = tmp.path().join("a.png");
        probe().save(&png).unwrap();
        assert!(
            !save_jpeg_orientation(&png, &tmp.path().join("o.jpg"), &[EditOp::RotateRight])
                .unwrap()
        );
        let jpg = tmp.path().join("a.jpg");
        write_jpeg(&jpg, None);
        let crop = EditOp::Crop {
            x: 0,
            y: 0,
            width: 5,
            height: 5,
        };
        assert!(!save_jpeg_orientation(&jpg, &tmp.path().join("o.jpg"), &[crop]).unwrap());
        assert!(save_jpeg_orientation(
            &tmp.path().join("missing.jpg"),
            &jpg,
            &[EditOp::RotateLeft]
        )
        .is_err());
    }

    #[test]
    fn save_as_preserves_exif_and_resets_orientation() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.jpg");
        write_jpeg(&src, Some(crate::meta::minimal_exif(6)));
        let upright = decode(&src);
        assert_eq!(upright.dimensions(), (32, 64));
        assert!(
            is_red(upright.get_pixel(16, 5)),
            "orientation 6 puts the left (red) half on top"
        );

        for (name, format) in [
            ("out.jpg", SaveFormat::Jpeg),
            ("out.png", SaveFormat::Png),
            ("out.webp", SaveFormat::WebP),
        ] {
            let dst = tmp.path().join(name);
            save_image(&upright, &dst, format, 92, Some(&src)).unwrap();
            let again = decode(&dst);
            assert_eq!(again.dimensions(), (32, 64), "{name}: not rotated twice");
            assert!(is_red(again.get_pixel(16, 5)), "{name}");
            let container = DynImage::from_bytes(Bytes::from(std::fs::read(&dst).unwrap()))
                .unwrap()
                .unwrap();
            let exif = container.exif().expect("EXIF carried over");
            let at = find_orientation_value(&exif).unwrap();
            assert_eq!(exif[at], 1, "{name}: orientation reset");
        }
    }

    #[test]
    fn save_formats_round_trip_and_flatten_alpha_for_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        let img = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        for format in SaveFormat::ALL {
            let dst = tmp.path().join(format!("x.{}", format.extension()));
            save_image(&img, &dst, format, 90, None).unwrap();
            assert_eq!(SaveFormat::from_path(&dst), Some(format));
            let back = decode(&dst);
            assert_eq!(back.dimensions(), (8, 8), "{format:?}");
            if format == SaveFormat::Jpeg {
                assert!(
                    back.get_pixel(4, 4).0[0] > 250,
                    "transparent becomes white in JPEG"
                );
            }
        }
        assert_eq!(SaveFormat::from_path(Path::new("a.xyz")), None);
        // No stray temp files are left behind.
        let leftovers = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("ff-tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
