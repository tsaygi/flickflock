//! Thumbnail generation: lazy, parallel, disk-cached, and using embedded
//! EXIF thumbnails when they are good enough.

use crate::cache::ThumbDiskCache;
use crate::decode::{DecodeOptions, Decoders, Format};
use crate::jobs::{CancelToken, JobQueue};
use crate::loader::downscale;
use crossbeam_channel::Receiver;
use image::RgbaImage;
use std::path::{Path, PathBuf};

/// Longest side of generated thumbnails, in pixels.
pub const THUMB_EDGE: u32 = 256;

/// Embedded EXIF thumbnails at least this large are used as-is.
const MIN_EMBEDDED_EDGE: u32 = 160;

#[derive(Debug)]
pub struct ThumbEvent {
    pub path: PathBuf,
    /// Straight-alpha RGBA, longest side ≤ [`THUMB_EDGE`]. `None` when the file could not be decoded.
    pub image: Option<RgbaImage>,
}

/// Produce a thumbnail for `path` (blocking). Order of preference: disk
/// cache, embedded EXIF thumbnail, full decode + downscale.
pub fn make_thumbnail(
    path: &Path,
    decoders: &Decoders,
    cache: Option<&ThumbDiskCache>,
) -> Option<RgbaImage> {
    if let Some(hit) = cache.and_then(|c| c.get(path, THUMB_EDGE)) {
        return Some(hit);
    }
    let thumb = match embedded_thumbnail(path) {
        Some(t) => t,
        None => {
            let opts = DecodeOptions {
                animate: false,
                max_dim: Some(THUMB_EDGE),
            };
            let decoded = decoders.decode(path, &opts).ok()?;
            downscale(decoded.image, THUMB_EDGE)
        }
    };
    if let Some(c) = cache {
        c.put(path, THUMB_EDGE, &thumb);
    }
    Some(thumb)
}

/// The EXIF thumbnail of a JPEG, upright, if it exists and is big enough.
fn embedded_thumbnail(path: &Path) -> Option<RgbaImage> {
    if crate::decode::detect_format(path).ok()? != Format::Jpeg {
        return None;
    }
    let (jpeg, orientation) = crate::meta::exif_thumbnail(path)?;
    let mut img = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).ok()?;
    if img.width().max(img.height()) < MIN_EMBEDDED_EDGE {
        return None;
    }
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Some(downscale(img.into_rgba8(), THUMB_EDGE))
}

/// Background thumbnail service driven by a wanted list: call
/// [`ThumbService::set_wanted`] with the thumbnails currently visible (most
/// important first); anything else that was queued is forgotten.
pub struct ThumbService {
    queue: JobQueue<PathBuf, ()>,
    rx: Receiver<ThumbEvent>,
}

impl ThumbService {
    pub fn new(
        decoders: Decoders,
        cache: Option<ThumbDiskCache>,
        threads: usize,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let queue = JobQueue::new(
            "ff-thumb",
            threads,
            move |path: &PathBuf, _: (), cancel: &CancelToken| {
                if cancel.is_cancelled() {
                    return;
                }
                let image = make_thumbnail(path, &decoders, cache.as_ref());
                // Deliver even when cancelled: the work is done and the result is still valid.
                if tx
                    .send(ThumbEvent {
                        path: path.clone(),
                        image,
                    })
                    .is_ok()
                {
                    wake();
                }
            },
        );
        Self { queue, rx }
    }

    pub fn set_wanted(&self, paths: Vec<PathBuf>) {
        self.queue
            .set_wanted(paths.into_iter().map(|p| (p, ())).collect());
    }

    pub fn poll(&self) -> Vec<ThumbEvent> {
        self.rx.try_iter().collect()
    }

    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<ThumbEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    use std::time::Duration;

    #[test]
    fn generates_caches_and_reuses() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("photo.png");
        RgbaImage::from_pixel(1024, 512, Rgba([9, 99, 199, 255]))
            .save(&src)
            .unwrap();
        let cache = ThumbDiskCache::new(tmp.path().join("cache"));
        let t = make_thumbnail(&src, &Decoders::builtin(), Some(&cache)).unwrap();
        assert_eq!(t.dimensions(), (256, 128));
        assert_eq!(t.get_pixel(100, 50).0, [9, 99, 199, 255]);
        assert_eq!(cache.get(&src, THUMB_EDGE).as_ref(), Some(&t));
        // With an empty decoder list only the cache can answer.
        let none = Decoders::builtin();
        assert_eq!(make_thumbnail(&src, &none, Some(&cache)).unwrap(), t);
        assert!(make_thumbnail(&tmp.path().join("nope.png"), &none, Some(&cache)).is_none());
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("tiny.png");
        RgbaImage::new(20, 10).save(&src).unwrap();
        assert_eq!(
            make_thumbnail(&src, &Decoders::builtin(), None)
                .unwrap()
                .dimensions(),
            (20, 10)
        );
    }

    #[test]
    fn service_reports_successes_and_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.png");
        RgbaImage::new(40, 40).save(&good).unwrap();
        let bad = tmp.path().join("bad.png");
        std::fs::write(&bad, b"junk").unwrap();
        let svc = ThumbService::new(Decoders::builtin(), None, 2, || {});
        svc.set_wanted(vec![good.clone(), bad.clone()]);
        let mut results = std::collections::HashMap::new();
        for _ in 0..2 {
            let ev = svc
                .recv_timeout(Duration::from_secs(10))
                .expect("thumbnail result");
            results.insert(ev.path, ev.image.is_some());
        }
        assert!(results[&good]);
        assert!(!results[&bad]);
    }
}
