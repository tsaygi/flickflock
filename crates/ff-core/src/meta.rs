//! Metadata extraction (file facts, EXIF, GPS, XMP, ICC profile name) and
//! helpers used by sorting, thumbnails and RAW previews.

use exif::{Context, In, Tag, Value};
use image::metadata::Orientation;
use image::ImageDecoder;
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;

/// One row of the info panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// "File", "Image", "EXIF", "GPS" or "XMP".
    pub group: &'static str,
    pub name: String,
    pub value: String,
}

impl Property {
    fn new(group: &'static str, name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            group,
            name: name.into(),
            value: value.into(),
        }
    }

    /// Stable identifier used for pinning, e.g. `"EXIF/ExposureTime"`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.group, self.name)
    }
}

/// Facts about the decoded image that the caller already knows.
#[derive(Debug, Clone, Default)]
pub struct ImageFacts {
    pub dimensions: Option<(u32, u32)>,
    pub format: Option<&'static str>,
    pub frames: Option<usize>,
}

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    exif::Reader::new().read_from_container(&mut reader).ok()
}

/// EXIF DateTimeOriginal (or DateTime) as "YYYY:MM:DD HH:MM:SS".
pub fn date_taken(path: &Path) -> Option<String> {
    let exif = read_exif(path)?;
    [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime]
        .iter()
        .find_map(|tag| {
            let field = exif.get_field(*tag, In::PRIMARY)?;
            match &field.value {
                Value::Ascii(v) => {
                    let s = String::from_utf8_lossy(v.first()?).trim().to_owned();
                    (s.len() >= 10 && !s.starts_with("0000")).then_some(s)
                }
                _ => None,
            }
        })
}

fn orientation_of(exif: &exif::Exif) -> Option<Orientation> {
    let v = exif
        .get_field(Tag::Orientation, In::PRIMARY)?
        .value
        .get_uint(0)?;
    Orientation::from_exif(u8::try_from(v).ok()?)
}

/// Orientation stored in a TIFF-like container held in memory (RAW files).
pub fn container_orientation(bytes: &[u8]) -> Option<Orientation> {
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()?;
    orientation_of(&exif)
}

/// The JPEG thumbnail embedded in a file's EXIF data, with the main image's orientation.
pub fn exif_thumbnail(path: &Path) -> Option<(Vec<u8>, Option<Orientation>)> {
    let exif = read_exif(path)?;
    let offset = exif
        .get_field(Tag::JPEGInterchangeFormat, In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let len = exif
        .get_field(Tag::JPEGInterchangeFormatLength, In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let data = exif.buf().get(offset..offset.checked_add(len)?)?;
    data.starts_with(&[0xFF, 0xD8])
        .then(|| (data.to_vec(), orientation_of(&exif)))
}

/// Everything the info panel shows for a file. Never fails: sections that
/// cannot be read are simply absent.
pub fn extract(path: &Path, facts: &ImageFacts) -> Vec<Property> {
    let mut out = Vec::new();
    if let Some(name) = path.file_name() {
        out.push(Property::new("File", "Name", name.to_string_lossy()));
    }
    if let Some(dir) = path.parent() {
        out.push(Property::new("File", "Folder", dir.to_string_lossy()));
    }
    if let Ok(md) = std::fs::metadata(path) {
        out.push(Property::new("File", "Size", human_size(md.len())));
        if let Ok(t) = md.modified() {
            out.push(Property::new(
                "File",
                "Modified",
                exif_to_iso(&crate::folder::format_system_time(t)) + " UTC",
            ));
        }
        if let Ok(t) = md.created() {
            out.push(Property::new(
                "File",
                "Created",
                exif_to_iso(&crate::folder::format_system_time(t)) + " UTC",
            ));
        }
    }
    if let Some(format) = facts.format {
        out.push(Property::new("Image", "Format", format));
    }
    if let Some((w, h)) = facts
        .dimensions
        .or_else(|| crate::decode::probe_dimensions(path))
    {
        out.push(Property::new("Image", "Dimensions", format!("{w} × {h}")));
        out.push(Property::new(
            "Image",
            "Megapixels",
            format!("{:.1} MP", f64::from(w) * f64::from(h) / 1e6),
        ));
    }
    if let Some(frames) = facts.frames.filter(|n| *n > 1) {
        out.push(Property::new("Image", "Frames", frames.to_string()));
    }
    if let Some(name) = icc_profile_name(path) {
        out.push(Property::new("Image", "Color profile", name));
    }
    if let Some(exif) = read_exif(path) {
        exif_properties(&exif, &mut out);
    }
    xmp_properties(path, &mut out);
    out
}

fn exif_properties(exif: &exif::Exif, out: &mut Vec<Property>) {
    for field in exif.fields().filter(|f| f.ifd_num == In::PRIMARY) {
        let group = if field.tag.context() == Context::Gps {
            "GPS"
        } else {
            "EXIF"
        };
        // Skip bulky binary blobs and structural pointers.
        if matches!(
            field.tag,
            Tag::MakerNote
                | Tag::UserComment
                | Tag::ExifIFDPointer
                | Tag::GPSInfoIFDPointer
                | Tag::InteropIFDPointer
        ) {
            continue;
        }
        if let Value::Undefined(bytes, _) = &field.value {
            if bytes.len() > 64 {
                continue;
            }
        }
        let mut value = field.display_value().with_unit(exif).to_string();
        if value.len() > 300 {
            let cut = (0..=300)
                .rev()
                .find(|i| value.is_char_boundary(*i))
                .unwrap_or(0);
            value.truncate(cut);
            value.push('…');
        }
        let name = field.tag.to_string();
        if !out.iter().any(|p| p.group == group && p.name == name) {
            out.push(Property::new(group, name, value));
        }
    }
    if let (Some(lat), Some(lon)) = (
        gps_decimal(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S'),
        gps_decimal(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W'),
    ) {
        out.push(Property::new(
            "GPS",
            "Coordinates",
            format!("{lat:.6}, {lon:.6}"),
        ));
    }
}

fn gps_decimal(exif: &exif::Exif, tag: Tag, ref_tag: Tag, negative: u8) -> Option<f64> {
    let Value::Rational(parts) = &exif.get_field(tag, In::PRIMARY)?.value else {
        return None;
    };
    if parts.len() < 3 {
        return None;
    }
    let deg = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
    let sign = match &exif.get_field(ref_tag, In::PRIMARY)?.value {
        Value::Ascii(v) if v.first().and_then(|s| s.first()) == Some(&negative) => -1.0,
        _ => 1.0,
    };
    deg.is_finite().then_some(deg * sign)
}

/// "2024:05:01 10:00:00" → "2024-05-01 10:00:00".
fn exif_to_iso(s: &str) -> String {
    match (s.get(0..4), s.get(5..7), s.get(8..)) {
        (Some(y), Some(m), Some(rest)) => format!("{y}-{m}-{rest}"),
        _ => s.to_owned(),
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Description of the embedded ICC profile, if the file has one.
pub fn icc_profile_name(path: &Path) -> Option<String> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let icc = std::panic::catch_unwind(|| reader.into_decoder().ok()?.icc_profile().ok().flatten())
        .ok()??;
    icc_description(&icc)
}

/// Read the `desc` tag of an ICC profile (v2 `desc` or v4 `mluc`).
pub fn icc_description(icc: &[u8]) -> Option<String> {
    let u32_at = |o: usize| {
        icc.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let count = u32_at(128)?.min(256);
    for i in 0..count {
        let entry = 132 + i * 12;
        if icc.get(entry..entry + 4)? != b"desc" {
            continue;
        }
        let (offset, size) = (u32_at(entry + 4)?, u32_at(entry + 8)?);
        let tag = icc.get(offset..offset.checked_add(size)?)?;
        let text = match tag.get(0..4)? {
            b"desc" => {
                let len = u32::from_be_bytes(tag.get(8..12)?.try_into().ok()?) as usize;
                String::from_utf8_lossy(tag.get(12..12 + len)?)
                    .trim_end_matches('\0')
                    .to_owned()
            }
            b"mluc" => {
                let len = u32::from_be_bytes(tag.get(20..24)?.try_into().ok()?) as usize;
                let off = u32::from_be_bytes(tag.get(24..28)?.try_into().ok()?) as usize;
                let utf16: Vec<u16> = tag
                    .get(off..off.checked_add(len)?)?
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&utf16)
            }
            _ => return None,
        };
        let text = text.trim().to_owned();
        return (!text.is_empty()).then_some(text);
    }
    None
}

/// How much of the file head is searched for an XMP packet.
const XMP_SCAN_BYTES: u64 = 1 << 20;
const MAX_XMP_PROPERTIES: usize = 80;

fn xmp_properties(path: &Path, out: &mut Vec<Property>) {
    let Ok(file) = File::open(path) else { return };
    let mut head = Vec::new();
    if file.take(XMP_SCAN_BYTES).read_to_end(&mut head).is_err() {
        return;
    }
    let Some(packet) = find_xmp_packet(&head) else {
        return;
    };
    for (name, value) in parse_xmp(&packet).into_iter().take(MAX_XMP_PROPERTIES) {
        if !out.iter().any(|p| p.group == "XMP" && p.name == name) {
            out.push(Property::new("XMP", name, value));
        }
    }
}

fn find_xmp_packet(data: &[u8]) -> Option<String> {
    let find = |needle: &[u8], from: usize| {
        data[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    };
    let start = find(b"<x:xmpmeta", 0)?;
    let end = find(b"</x:xmpmeta>", start)?;
    Some(String::from_utf8_lossy(&data[start..end]).into_owned())
}

/// A deliberately small XMP reader: collects `ns:Name="value"` attributes and
/// `<ns:Name>text</ns:Name>` simple elements. Structured values (lists,
/// nested resources) are flattened to their text items.
pub fn parse_xmp(xml: &str) -> Vec<(String, String)> {
    let mut props: Vec<(String, String)> = Vec::new();
    fn push(props: &mut Vec<(String, String)>, name: &str, value: &str) {
        let value = unescape_xml(value.trim());
        let skip = name.starts_with("xmlns:")
            || name.starts_with("rdf:")
            || name.starts_with("x:")
            || name.starts_with("xml:")
            || value.is_empty();
        if !skip && !props.iter().any(|(n, _)| n == name) {
            props.push((name.to_owned(), value));
        }
    }
    let mut rest = xml;
    // Name of the innermost property element, used to label rdf:li items.
    let mut current_prop: Option<String> = None;
    while let Some(lt) = rest.find('<') {
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else { break };
        let tag = &after[..gt];
        let body = &after[gt + 1..];
        rest = body;
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let (name, attrs) = match tag.find(char::is_whitespace) {
            Some(i) => (&tag[..i], &tag[i..]),
            None => (tag, ""),
        };
        for (attr, value) in parse_attributes(attrs) {
            push(&mut props, attr, value);
        }
        if self_closing {
            continue;
        }
        let text_end = body.find('<').unwrap_or(body.len());
        let text = &body[..text_end];
        if name == "rdf:li" {
            if let Some(prop) = &current_prop {
                if !text.trim().is_empty() {
                    let value = unescape_xml(text.trim());
                    match props.iter_mut().find(|(n, _)| n == prop) {
                        Some((_, existing)) => {
                            existing.push_str(", ");
                            existing.push_str(&value);
                        }
                        None => props.push((prop.clone(), value)),
                    }
                }
            }
        } else if !name.starts_with("rdf:") && !name.starts_with("x:") && name.contains(':') {
            if text.trim().is_empty() {
                current_prop = Some(name.to_owned());
            } else if body[text_end..].starts_with("</") {
                let value = unescape_xml(text.trim());
                if !props.iter().any(|(n, _)| n == name) {
                    props.push((name.to_owned(), value));
                }
            }
        }
    }
    props
}

fn parse_attributes(mut s: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        let Some(eq) = s.find('=') else { break };
        let name = s[..eq].trim();
        let after = s[eq + 1..].trim_start();
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let Some(end) = after[1..].find(quote) else {
            break;
        };
        out.push((name, &after[1..1 + end]));
        s = &after[end + 2..];
    }
    out
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Build a minimal EXIF (TIFF) block. Used by tests, fixtures and when a
/// JPEG without EXIF needs an orientation tag.
pub fn minimal_exif(orientation: u16) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"II*\0");
    t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
    t.extend_from_slice(&1u16.to_le_bytes()); // one entry
    t.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    t.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    t.extend_from_slice(&1u32.to_le_bytes()); // count
    t.extend_from_slice(&orientation.to_le_bytes());
    t.extend_from_slice(&[0, 0]);
    t.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn xmp_attributes_elements_and_lists() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="5" xmp:CreatorTool="Cam &amp; Co">
              <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Sunset</rdf:li></rdf:Alt></dc:title>
              <dc:subject><rdf:Bag><rdf:li>beach</rdf:li><rdf:li>sea</rdf:li></rdf:Bag></dc:subject>
              <photoshop:City>Lisbon</photoshop:City>
              <xmp:Empty/>
            </rdf:Description></rdf:RDF>"#;
        let props = parse_xmp(xml);
        let get = |n: &str| props.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        assert_eq!(get("xmp:Rating"), Some("5"));
        assert_eq!(get("xmp:CreatorTool"), Some("Cam & Co"));
        assert_eq!(get("dc:title"), Some("Sunset"));
        assert_eq!(get("dc:subject"), Some("beach, sea"));
        assert_eq!(get("photoshop:City"), Some("Lisbon"));
        assert!(get("xmlns:xmp").is_none() && get("rdf:about").is_none());
        assert!(parse_xmp("<broken").is_empty());
        assert!(parse_xmp("").is_empty());
    }

    #[test]
    fn minimal_exif_is_readable() {
        let exif = exif::Reader::new().read_raw(minimal_exif(6)).unwrap();
        assert_eq!(orientation_of(&exif), Some(Orientation::Rotate90));
        assert_eq!(
            container_orientation(&minimal_exif(3)),
            Some(Orientation::Rotate180)
        );
        assert_eq!(container_orientation(b"not tiff"), None);
    }

    #[test]
    fn icc_description_v2_and_garbage() {
        let mut icc = vec![0u8; 128];
        icc.extend_from_slice(&1u32.to_be_bytes());
        icc.extend_from_slice(b"desc");
        icc.extend_from_slice(&144u32.to_be_bytes());
        icc.extend_from_slice(&23u32.to_be_bytes());
        icc.extend_from_slice(b"desc\0\0\0\0");
        icc.extend_from_slice(&11u32.to_be_bytes());
        icc.extend_from_slice(b"Display P3\0");
        assert_eq!(icc_description(&icc).as_deref(), Some("Display P3"));
        assert_eq!(icc_description(&[0u8; 10]), None);
        assert_eq!(icc_description(&[0xFFu8; 200]), None);
    }

    #[test]
    fn extract_reports_file_and_image_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("pic.png");
        image::RgbaImage::new(40, 20).save(&p).unwrap();
        let props = extract(
            &p,
            &ImageFacts {
                format: Some("PNG"),
                ..Default::default()
            },
        );
        let get = |k: &str| props.iter().find(|p| p.key() == k).map(|p| p.value.clone());
        assert_eq!(get("File/Name").as_deref(), Some("pic.png"));
        assert_eq!(get("Image/Dimensions").as_deref(), Some("40 × 20"));
        assert_eq!(get("Image/Format").as_deref(), Some("PNG"));
        assert!(get("File/Size").is_some() && get("File/Modified").is_some());
        // Unreadable files still produce the basics without panicking.
        assert!(!extract(&tmp.path().join("gone.jpg"), &ImageFacts::default()).is_empty());
    }
}
