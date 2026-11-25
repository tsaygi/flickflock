//! Background image loading: decode, downscale to display size, split into
//! GPU-sized tiles with mip chains, all off the UI thread and cancellable.

use crate::anim::{AnimatedImage, DEFAULT_RAW_FRAME_BUDGET};
use crate::decode::{DecodeError, DecodeOptions, Decoders, Format};
use crate::jobs::{CancelToken, JobQueue};
use crossbeam_channel::{Receiver, Sender};
use image::RgbaImage;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Which representation of an image is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Level {
    /// Downscaled to roughly the size of the display. Cheap to keep cached.
    Preview,
    /// Every pixel, tiled when larger than the GPU texture limit.
    Full,
}

#[derive(Debug, Clone, Copy)]
pub struct LoadJob {
    /// Longest side of a preview, in pixels.
    pub preview_dim: u32,
    /// Largest texture edge the GPU accepts; bigger images are tiled.
    pub tile_size: u32,
    /// Decode all frames of animated images.
    pub animate: bool,
}

/// One texture's worth of pixels: premultiplied RGBA8 with a full mip chain.
#[derive(Debug, Clone)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// `mips[0]` is `width × height`, each further level halves both (min 1).
    pub mips: Vec<Vec<u8>>,
}

/// An image ready for GPU upload.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<Tile>,
}

impl Prepared {
    /// Premultiply, tile and build mips for an image.
    pub fn from_rgba(image: RgbaImage, tile_size: u32, with_mips: bool) -> Self {
        let (width, height) = image.dimensions();
        let mut raw = image.into_raw();
        premultiply(&mut raw);
        let tile_size = tile_size.max(64);
        let mut tiles = Vec::new();
        if width <= tile_size && height <= tile_size {
            tiles.push(Tile {
                x: 0,
                y: 0,
                width,
                height,
                mips: build_mips(raw, width, height, with_mips),
            });
        } else {
            let mut y = 0;
            while y < height {
                let th = tile_size.min(height - y);
                let mut x = 0;
                while x < width {
                    let tw = tile_size.min(width - x);
                    let mut data = Vec::with_capacity(tw as usize * th as usize * 4);
                    for row in y..y + th {
                        let start = (row as usize * width as usize + x as usize) * 4;
                        data.extend_from_slice(&raw[start..start + tw as usize * 4]);
                    }
                    tiles.push(Tile {
                        x,
                        y,
                        width: tw,
                        height: th,
                        mips: build_mips(data, tw, th, with_mips),
                    });
                    x += tw;
                }
                y += th;
            }
        }
        Self {
            width,
            height,
            tiles,
        }
    }

    /// Bytes of pixel data held (≈ GPU memory once uploaded).
    pub fn bytes(&self) -> usize {
        self.tiles
            .iter()
            .flat_map(|t| t.mips.iter())
            .map(Vec::len)
            .sum()
    }
}

/// Premultiply straight-alpha RGBA8 in place (no-op for opaque images).
pub fn premultiply(rgba: &mut [u8]) {
    if rgba.chunks_exact(4).all(|p| p[3] == 255) {
        return;
    }
    rgba.par_chunks_mut(1 << 16).for_each(|chunk| {
        for p in chunk.chunks_exact_mut(4) {
            let a = u32::from(p[3]);
            if a != 255 {
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * a + 127) / 255) as u8;
                }
            }
        }
    });
}

fn mip_dim(v: u32) -> u32 {
    (v / 2).max(1)
}

fn build_mips(base: Vec<u8>, width: u32, height: u32, with_mips: bool) -> Vec<Vec<u8>> {
    let mut mips = vec![base];
    if !with_mips {
        return mips;
    }
    let (mut w, mut h) = (width, height);
    while w > 1 || h > 1 {
        let next = halve(mips.last().expect("at least the base level"), w, h);
        w = mip_dim(w);
        h = mip_dim(h);
        mips.push(next);
    }
    mips
}

/// 2× box-filter downsample of premultiplied RGBA8 (`w × h` → `max(w/2,1) × max(h/2,1)`).
pub fn halve(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let (dw, dh) = ((w / 2).max(1), (h / 2).max(1));
    let mut dst = vec![0u8; dw * dh * 4];
    dst.par_chunks_mut(dw * 4)
        .enumerate()
        .for_each(|(dy, row)| {
            let y0 = (dy * 2).min(h - 1);
            let y1 = (dy * 2 + 1).min(h - 1);
            let r0 = &src[y0 * w * 4..(y0 + 1) * w * 4];
            let r1 = &src[y1 * w * 4..(y1 + 1) * w * 4];
            for (dx, out) in row.chunks_exact_mut(4).enumerate() {
                let x0 = (dx * 2).min(w - 1) * 4;
                let x1 = (dx * 2 + 1).min(w - 1) * 4;
                for c in 0..4 {
                    let sum = u32::from(r0[x0 + c])
                        + u32::from(r0[x1 + c])
                        + u32::from(r1[x0 + c])
                        + u32::from(r1[x1 + c]);
                    out[c] = ((sum + 2) / 4) as u8;
                }
            }
        });
    dst
}

/// High-quality downscale so the longest side is at most `max_dim`: repeated
/// 2× box filtering, then a final bilinear step of less than 2×.
/// Returns the image unchanged when it is already small enough.
pub fn downscale(image: RgbaImage, max_dim: u32) -> RgbaImage {
    let (w, h) = image.dimensions();
    let longest = w.max(h);
    if longest <= max_dim || max_dim == 0 {
        return image;
    }
    let (tw, th) = fit_within(w, h, max_dim);
    let (mut cw, mut ch) = (w, h);
    let mut data = image.into_raw();
    // Box filtering straight-alpha data would bleed colour from transparent
    // pixels, so work premultiplied and convert back at the end.
    let has_alpha = data.chunks_exact(4).any(|p| p[3] != 255);
    if has_alpha {
        premultiply(&mut data);
    }
    while cw / 2 >= tw && ch / 2 >= th {
        data = halve(&data, cw, ch);
        cw = mip_dim(cw);
        ch = mip_dim(ch);
    }
    let mut img = RgbaImage::from_raw(cw, ch, data).expect("buffer size matches");
    if (cw, ch) != (tw, th) {
        img = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle);
    }
    if has_alpha {
        unpremultiply(&mut img);
    }
    img
}

fn unpremultiply(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        if a != 0 && a != 255 {
            for c in &mut p[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

/// Scale `w × h` to fit a `max_dim` square, keeping the aspect ratio.
pub fn fit_within(w: u32, h: u32, max_dim: u32) -> (u32, u32) {
    let longest = w.max(h).max(1);
    if longest <= max_dim {
        return (w, h);
    }
    let scale = f64::from(max_dim) / f64::from(longest);
    (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    )
}

/// A finished load.
#[derive(Debug)]
pub struct Loaded {
    pub path: PathBuf,
    pub level: Level,
    pub format: Format,
    pub decoder: &'static str,
    /// Pixel size of the original image.
    pub full_size: (u32, u32),
    pub prepared: Prepared,
    /// `prepared` already contains every pixel (no separate full-resolution load needed).
    pub is_full_resolution: bool,
    pub animation: Option<Arc<AnimatedImage>>,
    /// More frames may exist than were decoded (reload with `animate`).
    pub maybe_animated: bool,
    pub decode_ms: u32,
}

#[derive(Debug)]
pub enum LoadEvent {
    Loaded(Box<Loaded>),
    Failed {
        path: PathBuf,
        level: Level,
        error: String,
        /// The OS refused to read the file (sandbox or privacy settings), as opposed to a
        /// missing or undecodable file.
        denied: bool,
    },
}

/// Decode `path` and prepare it for display. Runs on the calling thread.
pub fn load(
    path: &Path,
    level: Level,
    job: &LoadJob,
    decoders: &Decoders,
    cancel: &CancelToken,
) -> Option<LoadEvent> {
    let started = std::time::Instant::now();
    let fail = |error: String, denied: bool| {
        Some(LoadEvent::Failed {
            path: path.to_path_buf(),
            level,
            error,
            denied,
        })
    };
    if cancel.is_cancelled() {
        return None;
    }
    let opts = DecodeOptions {
        animate: job.animate,
        max_dim: (level == Level::Preview).then_some(job.preview_dim),
    };
    let decoded = match decoders.decode(path, &opts) {
        Ok(d) => d,
        Err(e) => {
            let denied = matches!(&e, DecodeError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied);
            return fail(e.to_string(), denied);
        }
    };
    if cancel.is_cancelled() {
        return None;
    }
    let full_size = decoded.full_size;
    let (format, decoder, maybe_animated) =
        (decoded.format, decoded.decoder, decoded.maybe_animated);
    // Animations are shown at native size from their frames; no preview scaling.
    let animation = if decoded.frames.len() > 1 {
        AnimatedImage::from_frames(decoded.frames, DEFAULT_RAW_FRAME_BUDGET).map(Arc::new)
    } else {
        None
    };
    let image = match level {
        Level::Preview if animation.is_none() => downscale(decoded.image, job.preview_dim),
        _ => decoded.image,
    };
    if cancel.is_cancelled() {
        return None;
    }
    let is_full_resolution = image.dimensions() == full_size;
    let prepared = Prepared::from_rgba(image, job.tile_size, true);
    if cancel.is_cancelled() {
        return None;
    }
    Some(LoadEvent::Loaded(Box::new(Loaded {
        path: path.to_path_buf(),
        level,
        format,
        decoder,
        full_size,
        prepared,
        is_full_resolution,
        animation,
        maybe_animated,
        decode_ms: started.elapsed().as_millis() as u32,
    })))
}

/// The loader service: a worker pool fed by a wanted list (see [`JobQueue`]).
pub struct Loader {
    queue: JobQueue<(PathBuf, Level), LoadJob>,
    rx: Receiver<LoadEvent>,
}

impl Loader {
    /// `wake` is called from worker threads whenever a result is ready (e.g.
    /// to request a repaint).
    pub fn new(
        decoders: Decoders,
        threads: usize,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx): (Sender<LoadEvent>, _) = crossbeam_channel::unbounded();
        let queue = JobQueue::new(
            "ff-load",
            threads,
            move |key: &(PathBuf, Level), job: LoadJob, cancel| {
                if let Some(event) = load(&key.0, key.1, &job, &decoders, cancel) {
                    if tx.send(event).is_ok() {
                        wake();
                    }
                }
            },
        );
        Self { queue, rx }
    }

    /// Declare what should be loaded, most important first. Everything else
    /// that is queued is dropped and running jobs for other images are cancelled.
    pub fn set_wanted(&self, wanted: Vec<((PathBuf, Level), LoadJob)>) {
        self.queue.set_wanted(wanted);
    }

    pub fn is_loading(&self, path: &Path, level: Level) -> bool {
        self.queue.is_active(&(path.to_path_buf(), level))
    }

    /// Drain finished loads (non-blocking).
    pub fn poll(&self) -> Vec<LoadEvent> {
        self.rx.try_iter().collect()
    }

    /// Wait for the next result (used by tests and benchmarks).
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<LoadEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    use std::time::Duration;

    const JOB: LoadJob = LoadJob {
        preview_dim: 64,
        tile_size: 8192,
        animate: true,
    };

    #[test]
    fn mip_chain_goes_down_to_one_pixel() {
        let img = RgbaImage::from_pixel(10, 3, Rgba([200, 100, 50, 255]));
        let p = Prepared::from_rgba(img, 8192, true);
        assert_eq!(p.tiles.len(), 1);
        let sizes: Vec<usize> = p.tiles[0].mips.iter().map(Vec::len).collect();
        assert_eq!(sizes, [10 * 3 * 4, 5 * 4, 2 * 4, 4]);
        assert_eq!(
            p.tiles[0].mips.last().unwrap().as_slice(),
            &[200, 100, 50, 255]
        );
        assert_eq!(p.bytes(), sizes.iter().sum::<usize>());
        let p = Prepared::from_rgba(RgbaImage::new(10, 3), 8192, false);
        assert_eq!(p.tiles[0].mips.len(), 1);
    }

    #[test]
    fn halve_averages_and_handles_odd_sizes() {
        // 3×1: [0, 100, 200] → 1×1 averaging the first two columns (clamped rows).
        let src = [0, 0, 0, 255, 100, 100, 100, 255, 200, 200, 200, 255];
        assert_eq!(halve(&src, 3, 1), [50, 50, 50, 255]);
        let src = [10, 0, 0, 255, 30, 0, 0, 255, 50, 0, 0, 255, 70, 0, 0, 255];
        assert_eq!(halve(&src, 2, 2), [40, 0, 0, 255]);
        assert_eq!(halve(&[1, 2, 3, 4], 1, 1), [1, 2, 3, 4]);
    }

    #[test]
    fn huge_images_are_tiled_without_gaps() {
        let img = RgbaImage::from_fn(300, 130, |x, y| Rgba([(x % 256) as u8, y as u8, 0, 255]));
        let p = Prepared::from_rgba(img.clone(), 128, true);
        assert_eq!((p.width, p.height), (300, 130));
        assert_eq!(p.tiles.len(), 3 * 2);
        let covered: u64 = p
            .tiles
            .iter()
            .map(|t| u64::from(t.width) * u64::from(t.height))
            .sum();
        assert_eq!(covered, 300 * 130);
        for t in &p.tiles {
            assert!(t.width <= 128 && t.height <= 128);
            // Every tile's top-left pixel matches the source.
            assert_eq!(&t.mips[0][..4], &img.get_pixel(t.x, t.y).0);
            let last = (t.width as usize * t.height as usize - 1) * 4;
            assert_eq!(
                &t.mips[0][last..last + 4],
                &img.get_pixel(t.x + t.width - 1, t.y + t.height - 1).0
            );
        }
        let last = p.tiles.last().unwrap();
        assert_eq!((last.x, last.y, last.width, last.height), (256, 128, 44, 2));
    }

    #[test]
    fn premultiplies_translucent_pixels() {
        let mut px = [255, 128, 0, 128, 10, 20, 30, 255, 99, 99, 99, 0];
        premultiply(&mut px);
        assert_eq!(px, [128, 64, 0, 128, 10, 20, 30, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn downscale_fits_and_keeps_aspect() {
        assert_eq!(fit_within(4000, 3000, 1000), (1000, 750));
        assert_eq!(fit_within(100, 50, 1000), (100, 50));
        assert_eq!(fit_within(10_000, 1, 100), (100, 1));
        let img = RgbaImage::from_pixel(1000, 500, Rgba([10, 200, 30, 255]));
        let small = downscale(img, 300);
        assert_eq!(small.dimensions(), (300, 150));
        assert_eq!(small.get_pixel(150, 75).0, [10, 200, 30, 255]);
        let same = downscale(RgbaImage::new(20, 10), 300);
        assert_eq!(same.dimensions(), (20, 10));
    }

    #[test]
    fn downscale_does_not_bleed_transparent_colour() {
        // Left half opaque red, right half fully transparent *green*.
        let img = RgbaImage::from_fn(64, 64, |x, _| {
            if x < 32 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 255, 0, 0])
            }
        });
        let small = downscale(img, 20);
        for p in small.pixels() {
            if p.0[3] > 0 {
                assert!(p.0[1] < 8 && p.0[0] > 247, "green bled into {:?}", p.0);
            }
        }
    }

    fn write_png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let p = dir.join(name);
        RgbaImage::from_fn(w, h, |x, y| Rgba([x as u8, y as u8, 7, 255]))
            .save(&p)
            .unwrap();
        p
    }

    #[test]
    fn preview_and_full_levels() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_png(tmp.path(), "big.png", 200, 100);
        let d = Decoders::builtin();
        let Some(LoadEvent::Loaded(l)) =
            load(&p, Level::Preview, &JOB, &d, &CancelToken::default())
        else {
            panic!()
        };
        assert_eq!(l.full_size, (200, 100));
        assert_eq!((l.prepared.width, l.prepared.height), (64, 32));
        assert!(!l.is_full_resolution);
        let Some(LoadEvent::Loaded(l)) = load(&p, Level::Full, &JOB, &d, &CancelToken::default())
        else {
            panic!()
        };
        assert_eq!((l.prepared.width, l.prepared.height), (200, 100));
        assert!(l.is_full_resolution);
        // Small images are complete at preview level already.
        let p = write_png(tmp.path(), "small.png", 40, 30);
        let Some(LoadEvent::Loaded(l)) =
            load(&p, Level::Preview, &JOB, &d, &CancelToken::default())
        else {
            panic!()
        };
        assert!(l.is_full_resolution);
    }

    #[test]
    fn cancelled_and_failed_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_png(tmp.path(), "a.png", 10, 10);
        let cancelled = CancelToken::default();
        cancelled.cancel();
        assert!(load(&p, Level::Preview, &JOB, &Decoders::builtin(), &cancelled).is_none());
        let bad = tmp.path().join("bad.png");
        std::fs::write(&bad, b"nope").unwrap();
        let ev = load(
            &bad,
            Level::Preview,
            &JOB,
            &Decoders::builtin(),
            &CancelToken::default(),
        );
        assert!(matches!(ev, Some(LoadEvent::Failed { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_reported_as_denied() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = write_png(tmp.path(), "locked.png", 10, 10);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&p).is_ok() {
            return; // running as root: nothing is unreadable
        }
        let ev = load(
            &p,
            Level::Preview,
            &JOB,
            &Decoders::builtin(),
            &CancelToken::default(),
        );
        assert!(matches!(ev, Some(LoadEvent::Failed { denied: true, .. })));
        let missing = tmp.path().join("missing.png");
        let ev = load(
            &missing,
            Level::Preview,
            &JOB,
            &Decoders::builtin(),
            &CancelToken::default(),
        );
        assert!(matches!(ev, Some(LoadEvent::Failed { denied: false, .. })));
    }

    #[test]
    fn service_delivers_results_and_wakes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_png(tmp.path(), "a.png", 30, 30);
        let b = tmp.path().join("missing.png");
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = woke.clone();
        let loader = Loader::new(Decoders::builtin(), 2, move || {
            w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        loader.set_wanted(vec![
            ((a.clone(), Level::Preview), JOB),
            ((b.clone(), Level::Preview), JOB),
        ]);
        let mut loaded = 0;
        let mut failed = 0;
        for _ in 0..2 {
            match loader
                .recv_timeout(Duration::from_secs(10))
                .expect("result")
            {
                LoadEvent::Loaded(l) => {
                    assert_eq!(l.path, a);
                    loaded += 1;
                }
                LoadEvent::Failed { path, .. } => {
                    assert_eq!(path, b);
                    failed += 1;
                }
            }
        }
        assert_eq!((loaded, failed), (1, 1));
        // Workers wake after sending, so the last wake can trail the last result.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while woke.load(std::sync::atomic::Ordering::SeqCst) < 2
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(woke.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(loader.poll().is_empty());
    }
}
