//! Image decoding: format detection by content, the [`Decoder`] trait, the
//! built-in pure-Rust decoders and embedded-preview extraction for camera RAW.

use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use std::sync::Arc;

/// Image container formats FlickFlock knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    WebP,
    Tiff,
    Bmp,
    Ico,
    Tga,
    Pnm,
    Qoi,
    Exr,
    Hdr,
    Dds,
    Farbfeld,
    Jxl,
    Psd,
    Avif,
    Heif,
    Raw,
    Unknown,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Gif => "GIF",
            Self::WebP => "WebP",
            Self::Tiff => "TIFF",
            Self::Bmp => "BMP",
            Self::Ico => "ICO",
            Self::Tga => "TGA",
            Self::Pnm => "PNM",
            Self::Qoi => "QOI",
            Self::Exr => "OpenEXR",
            Self::Hdr => "Radiance HDR",
            Self::Dds => "DDS",
            Self::Farbfeld => "Farbfeld",
            Self::Jxl => "JPEG XL",
            Self::Psd => "PSD",
            Self::Avif => "AVIF",
            Self::Heif => "HEIF",
            Self::Raw => "Camera RAW",
            Self::Unknown => "Unknown",
        }
    }

    fn image_format(self) -> Option<ImageFormat> {
        Some(match self {
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Png => ImageFormat::Png,
            Self::Gif => ImageFormat::Gif,
            Self::WebP => ImageFormat::WebP,
            Self::Tiff => ImageFormat::Tiff,
            Self::Bmp => ImageFormat::Bmp,
            Self::Ico => ImageFormat::Ico,
            Self::Tga => ImageFormat::Tga,
            Self::Pnm => ImageFormat::Pnm,
            Self::Qoi => ImageFormat::Qoi,
            Self::Exr => ImageFormat::OpenExr,
            Self::Hdr => ImageFormat::Hdr,
            Self::Dds => ImageFormat::Dds,
            Self::Farbfeld => ImageFormat::Farbfeld,
            Self::Avif => ImageFormat::Avif,
            _ => return None,
        })
    }
}

const RAW_EXTENSIONS: &[&str] = &[
    "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "dng", "rw2", "pef", "srw",
];

/// Every file extension (lower-case) that is indexed when browsing a folder.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "tif", "tiff", "bmp", "dib", "ico",
    "cur", "tga", "pbm", "pgm", "ppm", "pnm", "pam", "qoi", "exr", "hdr", "dds", "ff", "farbfeld",
    "jxl", "psd", "avif", "heic", "heif", "hif", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2",
    "raf", "orf", "dng", "rw2", "pef", "srw",
];

/// File types as shown to the user when choosing default-app associations:
/// `(name, extensions, MIME type)`. Together they cover [`SUPPORTED_EXTENSIONS`].
pub const FILE_TYPE_GROUPS: &[(&str, &[&str], &str)] = &[
    ("JPEG", &["jpg", "jpeg", "jpe", "jfif"], "image/jpeg"),
    ("PNG", &["png", "apng"], "image/png"),
    ("GIF", &["gif"], "image/gif"),
    ("WebP", &["webp"], "image/webp"),
    ("HEIF / HEIC", &["heic", "heif", "hif"], "image/heic"),
    ("AVIF", &["avif"], "image/avif"),
    ("JPEG XL", &["jxl"], "image/jxl"),
    ("TIFF", &["tif", "tiff"], "image/tiff"),
    ("BMP", &["bmp", "dib"], "image/bmp"),
    ("ICO", &["ico", "cur"], "image/vnd.microsoft.icon"),
    ("TGA", &["tga"], "image/x-tga"),
    ("PSD", &["psd"], "image/vnd.adobe.photoshop"),
    (
        "Camera RAW",
        &[
            "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "dng", "rw2", "pef",
            "srw",
        ],
        "image/x-dcraw",
    ),
    ("OpenEXR", &["exr"], "image/x-exr"),
    ("Radiance HDR", &["hdr"], "image/vnd.radiance"),
    (
        "Netpbm",
        &["pbm", "pgm", "ppm", "pnm", "pam"],
        "image/x-portable-anymap",
    ),
    ("QOI", &["qoi"], "image/qoi"),
    ("DDS", &["dds"], "image/x-dds"),
    ("Farbfeld", &["ff", "farbfeld"], "image/x-farbfeld"),
];

/// Is this path indexed as an image (by extension, which is cheap enough for
/// big folders on network shares)? Actual decoding detects by content.
pub fn is_supported_path(path: &Path) -> bool {
    match path.extension() {
        Some(ext) => {
            let ext = ext.to_string_lossy().to_lowercase();
            SUPPORTED_EXTENSIONS.contains(&ext.as_str())
        }
        None => false,
    }
}

fn format_from_extension(path: &Path) -> Format {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" => Format::Jpeg,
        "png" | "apng" => Format::Png,
        "gif" => Format::Gif,
        "webp" => Format::WebP,
        "tif" | "tiff" => Format::Tiff,
        "bmp" | "dib" => Format::Bmp,
        "ico" | "cur" => Format::Ico,
        "tga" => Format::Tga,
        "pbm" | "pgm" | "ppm" | "pnm" | "pam" => Format::Pnm,
        "qoi" => Format::Qoi,
        "exr" => Format::Exr,
        "hdr" => Format::Hdr,
        "dds" => Format::Dds,
        "ff" | "farbfeld" => Format::Farbfeld,
        "jxl" => Format::Jxl,
        "psd" => Format::Psd,
        "avif" => Format::Avif,
        "heic" | "heif" | "hif" => Format::Heif,
        e if RAW_EXTENSIONS.contains(&e) => Format::Raw,
        _ => Format::Unknown,
    }
}

/// Detect a format from the first bytes of a file. `ext_hint` disambiguates
/// containers shared by several formats (TIFF-based RAW files) and formats
/// without a signature (TGA).
pub fn detect_format_bytes(head: &[u8], ext_hint: Format) -> Format {
    let h = head;
    let starts = |sig: &[u8]| h.len() >= sig.len() && &h[..sig.len()] == sig;
    if starts(b"\xFF\xD8\xFF") {
        return Format::Jpeg;
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Format::Png;
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Format::Gif;
    }
    if starts(b"RIFF") && h.len() >= 12 && &h[8..12] == b"WEBP" {
        return Format::WebP;
    }
    if starts(b"FUJIFILMCCD-RAW")
        || starts(b"IIRO")
        || starts(b"IIRS")
        || starts(b"MMOR")
        || starts(b"IIU\0")
    {
        return Format::Raw;
    }
    if starts(b"II*\0") || starts(b"MM\0*") || starts(b"II+\0") || starts(b"MM\0+") {
        // CR2 has "CR" at offset 8; other TIFF-based RAW formats can only be told apart by extension.
        if (h.len() >= 10 && &h[8..10] == b"CR") || ext_hint == Format::Raw {
            return Format::Raw;
        }
        return Format::Tiff;
    }
    if starts(b"8BPS") {
        return Format::Psd;
    }
    if starts(b"qoif") {
        return Format::Qoi;
    }
    if starts(b"\x76\x2F\x31\x01") {
        return Format::Exr;
    }
    if starts(b"#?RADIANCE") || starts(b"#?RGBE") {
        return Format::Hdr;
    }
    if starts(b"DDS ") {
        return Format::Dds;
    }
    if starts(b"farbfeld") {
        return Format::Farbfeld;
    }
    if starts(b"\xFF\x0A") || starts(b"\0\0\0\x0CJXL \r\n\x87\n") {
        return Format::Jxl;
    }
    if h.len() >= 12 && &h[4..8] == b"ftyp" {
        let brand = &h[8..12];
        if brand == b"crx " {
            return Format::Raw;
        }
        if brand == b"avif" || brand == b"avis" {
            return Format::Avif;
        }
        const HEIF_BRANDS: [&[u8; 4]; 8] = [
            b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1",
        ];
        if HEIF_BRANDS.iter().any(|b| brand == *b) {
            // "mif1" is the generic brand; AVIF files may use it with "avif" as a compatible brand.
            if brand == b"mif1" && h.windows(4).skip(4).any(|w| w == b"avif") {
                return Format::Avif;
            }
            return Format::Heif;
        }
    }
    if starts(b"BM") && matches!(ext_hint, Format::Bmp | Format::Unknown) {
        return Format::Bmp;
    }
    if (starts(b"\0\0\x01\0") || starts(b"\0\0\x02\0"))
        && matches!(ext_hint, Format::Ico | Format::Unknown)
    {
        return Format::Ico;
    }
    if h.len() >= 3 && h[0] == b'P' && (b'1'..=b'7').contains(&h[1]) && h[2].is_ascii_whitespace() {
        return Format::Pnm;
    }
    ext_hint
}

/// Detect the format of a file by reading its first bytes.
pub fn detect_format(path: &Path) -> std::io::Result<Format> {
    let mut head = [0u8; 64];
    let mut file = File::open(path)?;
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(detect_format_bytes(
        &head[..filled],
        format_from_extension(path),
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// No decoder is available for this format on this system. The message is user-facing.
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<image::ImageError> for DecodeError {
    fn from(e: image::ImageError) -> Self {
        match e {
            image::ImageError::IoError(io) => DecodeError::Io(io),
            other => DecodeError::Failed(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DecodeOptions {
    /// Decode all frames of animated images (otherwise only the first).
    pub animate: bool,
    /// The caller only needs an image whose longest side is about this many
    /// pixels; decoders that can decode at reduced size cheaply may do so.
    pub max_dim: Option<u32>,
}

/// One frame of an animation, fully composited.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub image: RgbaImage,
    pub delay_ms: u32,
}

/// The result of decoding a file: straight-alpha sRGB RGBA8, upright.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub format: Format,
    /// The (first) image. May be smaller than `full_size` when `max_dim` was given.
    pub image: RgbaImage,
    /// Pixel size of the full image after applying orientation.
    pub full_size: (u32, u32),
    /// All frames for animated images when `animate` was requested (empty for stills).
    pub frames: Vec<RawFrame>,
    /// The file may contain more frames than were decoded (decode again
    /// with `animate` to get them).
    pub maybe_animated: bool,
    /// Name of the decoder that produced this.
    pub decoder: &'static str,
}

impl Decoded {
    pub fn still(format: Format, image: RgbaImage, decoder: &'static str) -> Self {
        let full_size = image.dimensions();
        Self {
            format,
            image,
            full_size,
            frames: Vec::new(),
            maybe_animated: false,
            decoder,
        }
    }
}

/// A decoder for one or more formats. Implementations must be thread-safe;
/// `decode` is called from worker threads.
pub trait Decoder: Send + Sync {
    fn name(&self) -> &'static str;
    fn handles(&self, format: Format) -> bool;
    fn decode(
        &self,
        path: &Path,
        format: Format,
        opts: &DecodeOptions,
    ) -> Result<Decoded, DecodeError>;
}

/// The ordered list of decoders. Earlier entries win.
#[derive(Clone)]
pub struct Decoders {
    list: Vec<Arc<dyn Decoder>>,
}

impl Default for Decoders {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Decoders {
    /// The pure-Rust decoders shipped with ff-core.
    pub fn builtin() -> Self {
        Self {
            list: vec![
                Arc::new(ImageCrateDecoder),
                Arc::new(JxlDecoder),
                Arc::new(PsdDecoder),
                Arc::new(RawPreviewDecoder),
            ],
        }
    }

    /// Add a decoder that is tried after the existing ones (e.g. an OS codec).
    pub fn register(&mut self, decoder: Arc<dyn Decoder>) {
        self.list.push(decoder);
    }

    /// Detect the format and decode. Never panics: decoder panics on corrupt
    /// files are caught and reported as errors.
    pub fn decode(&self, path: &Path, opts: &DecodeOptions) -> Result<Decoded, DecodeError> {
        let format = detect_format(path)?;
        let mut last_err = None;
        for d in self.list.iter().filter(|d| d.handles(format)) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                d.decode(path, format, opts)
            }));
            match result {
                Ok(Ok(decoded)) => return Ok(decoded),
                Ok(Err(e)) => {
                    tracing::debug!("{} failed on {}: {e}", d.name(), path.display());
                    // Prefer a real failure over a generic "unsupported" from a later decoder.
                    if last_err.is_none() || !matches!(e, DecodeError::Unsupported(_)) {
                        last_err = Some(e);
                    }
                }
                Err(_) => {
                    last_err = Some(DecodeError::Failed(format!(
                        "{} crashed on this file",
                        d.name()
                    )))
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            DecodeError::Unsupported(match format {
                Format::Heif => "HEIC is not supported on this system".to_owned(),
                Format::Avif => "AVIF is not supported in this build".to_owned(),
                Format::Unknown => "Not a supported image format".to_owned(),
                other => format!("{} is not supported on this system", other.label()),
            })
        }))
    }
}

/// Allocation ceiling for a single decode (a 30,000 × 30,000 RGBA image is 3.6 GB).
const MAX_DECODE_ALLOC: u64 = 4 << 30;

fn limits() -> image::Limits {
    let mut l = image::Limits::no_limits();
    l.max_alloc = Some(MAX_DECODE_ALLOC);
    l
}

/// Decode with any `image`-crate decoder, applying ICC → sRGB and orientation.
fn finish_decoder(mut decoder: impl ImageDecoder) -> Result<RgbaImage, DecodeError> {
    let _ = decoder.set_limits(limits());
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let has_color = decoder.color_type().has_color();
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let mut rgba = to_rgba8(img);
    if let (Some(icc), true) = (icc, has_color) {
        icc_to_srgb(&mut rgba, &icc);
    }
    Ok(rgba)
}

/// Convert to RGBA8. Floating-point (HDR/EXR) images hold linear light, so
/// they are clamped and encoded with the sRGB transfer curve.
fn to_rgba8(img: DynamicImage) -> RgbaImage {
    match img {
        DynamicImage::ImageRgba8(buf) => buf,
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => {
            let f = img.into_rgba32f();
            let (w, h) = f.dimensions();
            let mut out = Vec::with_capacity(f.as_raw().len());
            for px in f.as_raw().chunks_exact(4) {
                out.extend_from_slice(&[
                    linear_to_srgb8(px[0]),
                    linear_to_srgb8(px[1]),
                    linear_to_srgb8(px[2]),
                    (px[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                ]);
            }
            RgbaImage::from_raw(w, h, out).expect("buffer size matches")
        }
        other => other.into_rgba8(),
    }
}

fn linear_to_srgb8(v: f32) -> u8 {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    let s = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

/// Convert RGBA8 pixels from an embedded ICC profile to sRGB in place.
/// Returns `false` (leaving the pixels untouched) when the profile is already
/// sRGB, unreadable or not an RGB profile.
pub fn icc_to_srgb(rgba: &mut RgbaImage, icc: &[u8]) -> bool {
    let Some(profile) = qcms::Profile::new_from_slice(icc, false) else {
        return false;
    };
    if profile.is_sRGB() {
        return false;
    }
    let srgb = qcms::Profile::new_sRGB();
    let Some(transform) = qcms::Transform::new(
        &profile,
        &srgb,
        qcms::DataType::RGBA8,
        qcms::Intent::Perceptual,
    ) else {
        return false;
    };
    transform.apply(rgba);
    true
}

/// Pixel dimensions without decoding the whole image (best effort).
pub fn probe_dimensions(path: &Path) -> Option<(u32, u32)> {
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    reader.into_dimensions().ok()
}

fn open(path: &Path) -> Result<BufReader<File>, DecodeError> {
    Ok(BufReader::with_capacity(1 << 16, File::open(path)?))
}

/// Collect animation frames, normalising delays the way browsers do
/// (delays of 10 ms or less become 100 ms).
fn collect_frames<'a>(decoder: impl AnimationDecoder<'a>) -> Result<Vec<RawFrame>, DecodeError> {
    let mut frames = Vec::new();
    for frame in decoder.into_frames() {
        let frame = match frame {
            Ok(f) => f,
            // Keep what we have from a truncated animation.
            Err(e) if !frames.is_empty() => {
                tracing::warn!("animation truncated after {} frames: {e}", frames.len());
                break;
            }
            Err(e) => return Err(e.into()),
        };
        let (num, den) = frame.delay().numer_denom_ms();
        let ms = if den == 0 {
            0
        } else {
            (num as f64 / den as f64).round() as u32
        };
        let delay_ms = if ms <= 10 { 100 } else { ms };
        frames.push(RawFrame {
            image: frame.into_buffer(),
            delay_ms,
        });
    }
    if frames.is_empty() {
        return Err(DecodeError::Failed("animation has no frames".into()));
    }
    Ok(frames)
}

/// Decoder for everything the `image` crate handles.
struct ImageCrateDecoder;

impl Decoder for ImageCrateDecoder {
    fn name(&self) -> &'static str {
        "image"
    }

    fn handles(&self, format: Format) -> bool {
        match format {
            Format::Avif => cfg!(feature = "avif"),
            other => other.image_format().is_some(),
        }
    }

    fn decode(
        &self,
        path: &Path,
        format: Format,
        opts: &DecodeOptions,
    ) -> Result<Decoded, DecodeError> {
        if opts.animate {
            let frames = match format {
                Format::Gif => Some(collect_frames(image::codecs::gif::GifDecoder::new(open(
                    path,
                )?)?)?),
                Format::WebP => {
                    let d = image::codecs::webp::WebPDecoder::new(open(path)?)?;
                    if d.has_animation() {
                        Some(collect_frames(d)?)
                    } else {
                        None
                    }
                }
                Format::Png => {
                    let d = image::codecs::png::PngDecoder::new(open(path)?)?;
                    if d.is_apng()? {
                        Some(collect_frames(d.apng()?)?)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(frames) = frames {
                let mut decoded = Decoded::still(format, frames[0].image.clone(), self.name());
                if frames.len() > 1 {
                    decoded.frames = frames;
                }
                return Ok(decoded);
            }
        }
        // Still decode. Note cheaply whether more frames exist so the caller
        // can ask for the animation only when the image is actually shown.
        let (image, maybe_animated) = match format {
            Format::Gif => (
                finish_decoder(image::codecs::gif::GifDecoder::new(open(path)?)?)?,
                true,
            ),
            Format::WebP => {
                let d = image::codecs::webp::WebPDecoder::new(open(path)?)?;
                let animated = d.has_animation();
                (finish_decoder(d)?, animated)
            }
            Format::Png => {
                let d = image::codecs::png::PngDecoder::new(open(path)?)?;
                let animated = d.is_apng().unwrap_or(false);
                (finish_decoder(d)?, animated)
            }
            _ => {
                let fmt = format
                    .image_format()
                    .ok_or_else(|| DecodeError::Unsupported("unsupported format".into()))?;
                (
                    finish_decoder(ImageReader::with_format(open(path)?, fmt).into_decoder()?)?,
                    false,
                )
            }
        };
        let mut decoded = Decoded::still(format, image, self.name());
        decoded.maybe_animated = maybe_animated;
        Ok(decoded)
    }
}

/// JPEG XL via `jxl-oxide`.
struct JxlDecoder;

impl Decoder for JxlDecoder {
    fn name(&self) -> &'static str {
        "jxl-oxide"
    }

    fn handles(&self, format: Format) -> bool {
        format == Format::Jxl
    }

    fn decode(
        &self,
        path: &Path,
        format: Format,
        _opts: &DecodeOptions,
    ) -> Result<Decoded, DecodeError> {
        let decoder = jxl_oxide::integration::JxlDecoder::new(open(path)?)?;
        Ok(Decoded::still(
            format,
            finish_decoder(decoder)?,
            self.name(),
        ))
    }
}

/// Photoshop documents: the flattened composite.
struct PsdDecoder;

impl Decoder for PsdDecoder {
    fn name(&self) -> &'static str {
        "psd"
    }

    fn handles(&self, format: Format) -> bool {
        format == Format::Psd
    }

    fn decode(
        &self,
        path: &Path,
        format: Format,
        _opts: &DecodeOptions,
    ) -> Result<Decoded, DecodeError> {
        let bytes = std::fs::read(path)?;
        let psd = psd::Psd::from_bytes(&bytes).map_err(|e| DecodeError::Failed(e.to_string()))?;
        let image = RgbaImage::from_raw(psd.width(), psd.height(), psd.rgba())
            .ok_or_else(|| DecodeError::Failed("PSD composite has an unexpected size".into()))?;
        Ok(Decoded::still(format, image, self.name()))
    }
}

/// Camera RAW: shows the largest JPEG preview embedded in the file.
struct RawPreviewDecoder;

impl Decoder for RawPreviewDecoder {
    fn name(&self) -> &'static str {
        "raw-preview"
    }

    fn handles(&self, format: Format) -> bool {
        format == Format::Raw
    }

    fn decode(
        &self,
        path: &Path,
        format: Format,
        _opts: &DecodeOptions,
    ) -> Result<Decoded, DecodeError> {
        let bytes = std::fs::read(path)?;
        let (start, _) = find_largest_embedded_jpeg(&bytes).ok_or_else(|| {
            DecodeError::Unsupported("This RAW file has no embedded preview".into())
        })?;
        let reader = ImageReader::with_format(Cursor::new(&bytes[start..]), ImageFormat::Jpeg);
        let mut decoder = reader.into_decoder()?;
        let _ = decoder.set_limits(limits());
        let icc = decoder.icc_profile().ok().flatten();
        // The RAW container's orientation is authoritative; previews often carry none.
        let orientation = crate::meta::container_orientation(&bytes)
            .or_else(|| decoder.orientation().ok())
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut img = DynamicImage::from_decoder(decoder)?;
        img.apply_orientation(orientation);
        let mut rgba = img.into_rgba8();
        if let Some(icc) = icc {
            icc_to_srgb(&mut rgba, &icc);
        }
        Ok(Decoded::still(format, rgba, self.name()))
    }
}

/// Scan `data` for embedded baseline/progressive JPEG streams and return the
/// `(offset, pixel_count)` of the largest one. Lossless-JPEG raw sensor data
/// is rejected because its dimensions cannot be probed as a normal JPEG.
pub fn find_largest_embedded_jpeg(data: &[u8]) -> Option<(usize, u64)> {
    const MAX_CANDIDATES: usize = 64;
    let mut best: Option<(usize, u64)> = None;
    let mut tried = 0;
    let mut i = 0;
    while i + 4 <= data.len() && tried < MAX_CANDIDATES {
        if data[i] == 0xFF && data[i + 1] == 0xD8 && data[i + 2] == 0xFF {
            // A real JPEG continues with APPn, DQT, DHT, COM or SOF markers.
            let marker = data[i + 3];
            if matches!(marker, 0xE0..=0xEF | 0xDB | 0xC4 | 0xFE | 0xC0 | 0xC2) {
                tried += 1;
                let reader = ImageReader::with_format(Cursor::new(&data[i..]), ImageFormat::Jpeg);
                if let Ok((w, h)) = reader.into_dimensions() {
                    let area = w as u64 * h as u64;
                    if best.map(|(_, a)| area > a).unwrap_or(true) {
                        best = Some((i, area));
                    }
                }
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn gradient(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            Rgba([
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                128,
                255,
            ])
        })
    }

    #[test]
    fn detects_formats_by_magic_not_extension() {
        let tmp = tempfile::tempdir().unwrap();
        // A PNG with a .jpg extension.
        let p = tmp.path().join("actually_png.jpg");
        gradient(8, 8)
            .save_with_format(&p, ImageFormat::Png)
            .unwrap();
        assert_eq!(detect_format(&p).unwrap(), Format::Png);
        let d = Decoders::builtin()
            .decode(&p, &DecodeOptions::default())
            .unwrap();
        assert_eq!(d.format, Format::Png);
        assert_eq!(d.image.dimensions(), (8, 8));
    }

    #[test]
    fn magic_table() {
        let u = Format::Unknown;
        assert_eq!(
            detect_format_bytes(b"\xFF\xD8\xFF\xE0....", u),
            Format::Jpeg
        );
        assert_eq!(detect_format_bytes(b"GIF89a....", u), Format::Gif);
        assert_eq!(
            detect_format_bytes(b"RIFF\0\0\0\0WEBPVP8 ", u),
            Format::WebP
        );
        assert_eq!(detect_format_bytes(b"II*\0\x08\0\0\0", u), Format::Tiff);
        assert_eq!(
            detect_format_bytes(b"II*\0\x10\0\0\0CR\x02\0", u),
            Format::Raw
        );
        assert_eq!(
            detect_format_bytes(b"II*\0\x08\0\0\0", Format::Raw),
            Format::Raw
        );
        assert_eq!(
            detect_format_bytes(b"\0\0\0\x18ftypheic\0\0\0\0", u),
            Format::Heif
        );
        assert_eq!(
            detect_format_bytes(b"\0\0\0\x1cftypavif\0\0\0\0", u),
            Format::Avif
        );
        assert_eq!(
            detect_format_bytes(b"\0\0\0\x1cftypmif1\0\0\0\0mif1avifmiaf", u),
            Format::Avif
        );
        assert_eq!(
            detect_format_bytes(b"\0\0\0\x18ftypcrx \0\0\0\0", u),
            Format::Raw
        );
        assert_eq!(detect_format_bytes(b"8BPS\0\x01", u), Format::Psd);
        assert_eq!(detect_format_bytes(b"\xFF\x0A....", u), Format::Jxl);
        assert_eq!(detect_format_bytes(b"P6\n2 2\n255\n", u), Format::Pnm);
        assert_eq!(detect_format_bytes(b"FUJIFILMCCD-RAW 0201", u), Format::Raw);
        assert_eq!(
            detect_format_bytes(b"\0\0\x02\0\0\0\0\0", Format::Tga),
            Format::Tga
        );
        assert_eq!(detect_format_bytes(b"random", Format::Tga), Format::Tga);
        assert_eq!(detect_format_bytes(b"", u), Format::Unknown);
    }

    #[test]
    fn round_trips_every_encodable_builtin_format() {
        let tmp = tempfile::tempdir().unwrap();
        let src = gradient(33, 17);
        for (ext, fmt) in [
            ("png", ImageFormat::Png),
            ("bmp", ImageFormat::Bmp),
            ("tiff", ImageFormat::Tiff),
            ("tga", ImageFormat::Tga),
            ("qoi", ImageFormat::Qoi),
            ("ff", ImageFormat::Farbfeld),
            ("webp", ImageFormat::WebP),
            ("ico", ImageFormat::Ico),
            ("gif", ImageFormat::Gif),
        ] {
            let p = tmp.path().join(format!("img.{ext}"));
            if fmt == ImageFormat::Farbfeld {
                // Farbfeld only stores 16-bit RGBA.
                DynamicImage::ImageRgba8(src.clone())
                    .into_rgba16()
                    .save_with_format(&p, fmt)
                    .unwrap();
            } else {
                src.save_with_format(&p, fmt).unwrap();
            }
            let d = Decoders::builtin()
                .decode(&p, &DecodeOptions::default())
                .unwrap_or_else(|e| panic!("{ext}: {e}"));
            assert_eq!(d.image.dimensions(), (33, 17), "{ext}");
            assert_eq!(d.full_size, (33, 17), "{ext}");
            if fmt != ImageFormat::Gif {
                assert_eq!(d.image.get_pixel(20, 10), src.get_pixel(20, 10), "{ext}");
            }
        }
        // Lossy / float formats: just check they decode to the right size.
        let p = tmp.path().join("img.jpg");
        DynamicImage::ImageRgba8(src.clone())
            .into_rgb8()
            .save(&p)
            .unwrap();
        assert_eq!(
            Decoders::builtin()
                .decode(&p, &DecodeOptions::default())
                .unwrap()
                .image
                .dimensions(),
            (33, 17)
        );
        for ext in ["exr", "hdr", "ppm"] {
            let p = tmp.path().join(format!("img.{ext}"));
            if ext == "ppm" {
                DynamicImage::ImageRgba8(src.clone())
                    .into_rgb8()
                    .save(&p)
                    .unwrap();
            } else {
                DynamicImage::ImageRgba8(src.clone())
                    .into_rgb32f()
                    .save(&p)
                    .unwrap();
            }
            let d = Decoders::builtin()
                .decode(&p, &DecodeOptions::default())
                .unwrap_or_else(|e| panic!("{ext}: {e}"));
            assert_eq!(d.image.dimensions(), (33, 17), "{ext}");
        }
    }

    #[test]
    fn float_images_are_srgb_encoded() {
        assert_eq!(linear_to_srgb8(0.0), 0);
        assert_eq!(linear_to_srgb8(1.0), 255);
        assert_eq!(linear_to_srgb8(7.5), 255);
        assert_eq!(linear_to_srgb8(f32::NAN), 0);
        assert_eq!(linear_to_srgb8(0.214), 127); // mid grey
    }

    #[test]
    fn animated_gif_frames_and_delays() {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("anim.gif");
        {
            let mut enc = GifEncoder::new(File::create(&p).unwrap());
            enc.set_repeat(Repeat::Infinite).unwrap();
            for i in 0..5u8 {
                let img = RgbaImage::from_pixel(16, 12, Rgba([i * 50, 0, 0, 255]));
                let delay = Delay::from_numer_denom_ms(if i == 0 { 0 } else { 40 * i as u32 }, 1);
                enc.encode_frame(Frame::from_parts(img, 0, 0, delay))
                    .unwrap();
            }
        }
        let all = Decoders::builtin()
            .decode(
                &p,
                &DecodeOptions {
                    animate: true,
                    max_dim: None,
                },
            )
            .unwrap();
        assert_eq!(all.frames.len(), 5);
        assert_eq!(all.frames[0].delay_ms, 100, "zero delay is normalised");
        assert_eq!(all.frames[2].delay_ms, 80);
        assert_eq!(all.image.dimensions(), (16, 12));
        let first = Decoders::builtin()
            .decode(&p, &DecodeOptions::default())
            .unwrap();
        assert!(first.frames.is_empty());
        assert!(first.maybe_animated && !all.maybe_animated);
    }

    #[test]
    fn corrupt_and_unknown_files_are_errors_not_panics() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("broken.png");
        std::fs::write(&p, b"\x89PNG\r\n\x1a\n garbage garbage garbage").unwrap();
        assert!(Decoders::builtin()
            .decode(&p, &DecodeOptions::default())
            .is_err());
        let p = tmp.path().join("text.xyz");
        std::fs::write(&p, b"hello").unwrap();
        assert!(matches!(
            Decoders::builtin().decode(&p, &DecodeOptions::default()),
            Err(DecodeError::Unsupported(_))
        ));
        let p = tmp.path().join("empty.jpg");
        std::fs::write(&p, b"").unwrap();
        assert!(Decoders::builtin()
            .decode(&p, &DecodeOptions::default())
            .is_err());
        let p = tmp.path().join("fake.heic");
        std::fs::write(&p, b"\0\0\0\x18ftypheic\0\0\0\0").unwrap();
        let err = Decoders::builtin()
            .decode(&p, &DecodeOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains("HEIC is not supported"), "{err}");
        assert!(Decoders::builtin()
            .decode(&tmp.path().join("missing.png"), &DecodeOptions::default())
            .is_err());
    }

    fn jpeg_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(gradient(w, h))
            .into_rgb8()
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
            .unwrap();
        out
    }

    #[test]
    fn raw_preview_picks_the_largest_embedded_jpeg() {
        // A fake TIFF-like RAW container: header, small thumbnail, junk, big preview, junk.
        let mut raw = b"II*\0\x08\0\0\0".to_vec();
        raw.extend_from_slice(&[0u8; 100]);
        raw.extend_from_slice(&jpeg_bytes(16, 12));
        raw.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xC4, 1, 2, 3]); // lossless-JPEG-looking junk
        let big_at = raw.len();
        raw.extend_from_slice(&jpeg_bytes(120, 80));
        raw.extend_from_slice(&[7u8; 300]);
        assert_eq!(find_largest_embedded_jpeg(&raw), Some((big_at, 120 * 80)));

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("shot.dng");
        std::fs::write(&p, &raw).unwrap();
        let d = Decoders::builtin()
            .decode(&p, &DecodeOptions::default())
            .unwrap();
        assert_eq!(d.format, Format::Raw);
        assert_eq!(d.image.dimensions(), (120, 80));

        let p = tmp.path().join("nopreview.nef");
        std::fs::write(&p, b"II*\0\x08\0\0\0 nothing here").unwrap();
        assert!(matches!(
            Decoders::builtin().decode(&p, &DecodeOptions::default()),
            Err(DecodeError::Unsupported(_))
        ));
    }

    #[test]
    fn type_groups_cover_every_supported_extension_once() {
        let mut grouped: Vec<&str> = FILE_TYPE_GROUPS
            .iter()
            .flat_map(|(_, exts, _)| exts.iter().copied())
            .collect();
        grouped.sort_unstable();
        let mut supported = SUPPORTED_EXTENSIONS.to_vec();
        supported.sort_unstable();
        assert_eq!(grouped, supported);
        assert!(FILE_TYPE_GROUPS
            .iter()
            .all(|(name, _, mime)| !name.is_empty() && mime.starts_with("image/")));
    }

    #[test]
    fn extension_list() {
        assert!(is_supported_path(Path::new("/a/b/C.JPG")));
        assert!(is_supported_path(Path::new("x.heic")));
        assert!(is_supported_path(Path::new("x.CR3")));
        assert!(!is_supported_path(Path::new("x.txt")));
        assert!(!is_supported_path(Path::new("noext")));
    }

    #[test]
    fn external_decoders_are_used_for_their_formats() {
        struct Fake;
        impl Decoder for Fake {
            fn name(&self) -> &'static str {
                "fake"
            }
            fn handles(&self, f: Format) -> bool {
                f == Format::Heif
            }
            fn decode(
                &self,
                _: &Path,
                f: Format,
                _: &DecodeOptions,
            ) -> Result<Decoded, DecodeError> {
                Ok(Decoded::still(f, RgbaImage::new(3, 2), "fake"))
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("fake.heic");
        std::fs::write(&p, b"\0\0\0\x18ftypheic\0\0\0\0").unwrap();
        let mut d = Decoders::builtin();
        d.register(Arc::new(Fake));
        let out = d.decode(&p, &DecodeOptions::default()).unwrap();
        assert_eq!((out.decoder, out.image.dimensions()), ("fake", (3, 2)));
    }
}
