//! Animated images: frame storage with a bounded memory footprint and a
//! playback clock supporting speed, reverse play, looping and frame stepping.

use crate::decode::RawFrame;
use std::sync::Arc;
use std::time::Duration;

/// Default budget for uncompressed animation frames (per animation).
pub const DEFAULT_RAW_FRAME_BUDGET: usize = 768 << 20;

enum StoredFrame {
    /// Premultiplied RGBA8.
    Raw(Arc<Vec<u8>>),
    /// QOI-compressed premultiplied RGBA8 (used when the animation would not fit the budget).
    Packed(Vec<u8>),
}

/// A fully decoded animation. Frames are composited (disposal already
/// applied) and stored as **premultiplied** RGBA8, ready for GPU upload.
///
/// When the raw frames would exceed the memory budget, every frame is kept
/// QOI-compressed instead and decompressed on access, so very long
/// animations stay frame-accurate with a bounded footprint.
pub struct AnimatedImage {
    pub width: u32,
    pub height: u32,
    frames: Vec<StoredFrame>,
    delays: Vec<Duration>,
    bytes: usize,
}

impl std::fmt::Debug for AnimatedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimatedImage")
            .field("size", &(self.width, self.height))
            .field("frames", &self.frames.len())
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl AnimatedImage {
    /// Build from decoded frames (straight alpha). `raw_budget` is the number
    /// of bytes of uncompressed frames allowed before switching to packed storage.
    pub fn from_frames(frames: Vec<RawFrame>, raw_budget: usize) -> Option<Self> {
        let first = frames.first()?;
        let (width, height) = first.image.dimensions();
        let frame_bytes = width as usize * height as usize * 4;
        let pack = frame_bytes.saturating_mul(frames.len()) > raw_budget;
        let mut stored = Vec::with_capacity(frames.len());
        let mut delays = Vec::with_capacity(frames.len());
        let mut bytes = 0;
        for f in frames {
            if f.image.dimensions() != (width, height) {
                continue;
            }
            delays.push(Duration::from_millis(u64::from(f.delay_ms.max(1))));
            let mut raw = f.image.into_raw();
            crate::loader::premultiply(&mut raw);
            let frame = if pack {
                match qoi::encode_to_vec(&raw, width, height) {
                    Ok(packed) => StoredFrame::Packed(packed),
                    Err(_) => StoredFrame::Raw(Arc::new(raw)),
                }
            } else {
                StoredFrame::Raw(Arc::new(raw))
            };
            bytes += match &frame {
                StoredFrame::Raw(r) => r.len(),
                StoredFrame::Packed(p) => p.len(),
            };
            stored.push(frame);
        }
        Some(Self {
            width,
            height,
            frames: stored,
            delays,
            bytes,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Approximate memory held by the frames.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn is_packed(&self) -> bool {
        matches!(self.frames.first(), Some(StoredFrame::Packed(_)))
    }

    pub fn delay(&self, i: usize) -> Duration {
        self.delays
            .get(i)
            .copied()
            .unwrap_or(Duration::from_millis(100))
    }

    pub fn total_duration(&self) -> Duration {
        self.delays.iter().sum()
    }

    /// Premultiplied RGBA8 pixels of frame `i`.
    pub fn frame(&self, i: usize) -> Option<Arc<Vec<u8>>> {
        match self.frames.get(i)? {
            StoredFrame::Raw(r) => Some(r.clone()),
            StoredFrame::Packed(p) => qoi::decode_to_vec(p).ok().map(|(_, data)| Arc::new(data)),
        }
    }
}

pub const MIN_SPEED: f32 = 0.25;
pub const MAX_SPEED: f32 = 4.0;

/// Playback state for one animation.
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    pub frame: usize,
    pub playing: bool,
    pub reverse: bool,
    pub speed: f32,
    pub looping: bool,
    /// Animation time already spent on the current frame.
    elapsed: Duration,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            frame: 0,
            playing: true,
            reverse: false,
            speed: 1.0,
            looping: true,
            elapsed: Duration::ZERO,
        }
    }
}

impl Player {
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    /// Jump to a frame (scrubbing). Does not change the playing state.
    pub fn seek(&mut self, frame: usize, frame_count: usize) {
        self.frame = frame.min(frame_count.saturating_sub(1));
        self.elapsed = Duration::ZERO;
    }

    /// Step one frame forward (`+1`) or back (`-1`), wrapping, and pause.
    pub fn step(&mut self, delta: i32, frame_count: usize) {
        if frame_count == 0 {
            return;
        }
        self.playing = false;
        let n = frame_count as i64;
        self.frame = (self.frame as i64 + i64::from(delta)).rem_euclid(n) as usize;
        self.elapsed = Duration::ZERO;
    }

    /// Advance the clock by `dt` of wall time. Returns `true` when the
    /// displayed frame changed.
    pub fn advance(&mut self, dt: Duration, anim: &AnimatedImage) -> bool {
        let count = anim.frame_count();
        if !self.playing || count < 2 {
            return false;
        }
        let before = self.frame;
        let mut budget =
            self.elapsed + scale(dt, f64::from(self.speed.clamp(MIN_SPEED, MAX_SPEED)));
        // Skip whole loops at once after a long stall (e.g. the window was hidden).
        let total = anim.total_duration();
        if self.looping && !total.is_zero() && budget > total * 2 {
            budget = Duration::from_nanos((budget.as_nanos() % total.as_nanos()) as u64);
        }
        loop {
            let delay = anim.delay(self.frame);
            if budget < delay {
                break;
            }
            budget -= delay;
            let at_end = if self.reverse {
                self.frame == 0
            } else {
                self.frame + 1 == count
            };
            if at_end {
                if !self.looping {
                    self.playing = false;
                    budget = Duration::ZERO;
                    break;
                }
                self.frame = if self.reverse { count - 1 } else { 0 };
            } else if self.reverse {
                self.frame -= 1;
            } else {
                self.frame += 1;
            }
        }
        self.elapsed = budget;
        self.frame != before
    }

    /// Wall time until the next frame is due (for scheduling a repaint).
    pub fn time_until_next(&self, anim: &AnimatedImage) -> Option<Duration> {
        if !self.playing || anim.frame_count() < 2 {
            return None;
        }
        let remaining = anim.delay(self.frame).saturating_sub(self.elapsed);
        Some(scale(
            remaining,
            1.0 / f64::from(self.speed.clamp(MIN_SPEED, MAX_SPEED)),
        ))
    }
}

/// Scale a duration with nanosecond rounding (avoids float drift in the clock).
fn scale(d: Duration, factor: f64) -> Duration {
    Duration::from_nanos((d.as_nanos() as f64 * factor).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn anim(delays: &[u32]) -> AnimatedImage {
        let frames = delays
            .iter()
            .enumerate()
            .map(|(i, d)| RawFrame {
                image: RgbaImage::from_pixel(4, 4, Rgba([i as u8, 0, 0, 255])),
                delay_ms: *d,
            })
            .collect();
        AnimatedImage::from_frames(frames, DEFAULT_RAW_FRAME_BUDGET).unwrap()
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn advances_by_frame_delays() {
        let a = anim(&[100, 50, 200]);
        let mut p = Player::default();
        assert!(!p.advance(99 * MS, &a));
        assert_eq!(p.frame, 0);
        assert!(p.advance(MS, &a));
        assert_eq!(p.frame, 1);
        assert_eq!(p.time_until_next(&a), Some(50 * MS));
        assert!(p.advance(50 * MS, &a));
        assert_eq!(p.frame, 2);
        assert!(p.advance(200 * MS, &a));
        assert_eq!(p.frame, 0, "loops");
        // A big jump crosses several frames at once.
        assert!(p.advance(160 * MS, &a));
        assert_eq!(p.frame, 2);
    }

    #[test]
    fn speed_scales_time() {
        let a = anim(&[100, 100, 100, 100]);
        let mut p = Player::default();
        p.set_speed(2.0);
        p.advance(100 * MS, &a);
        assert_eq!(p.frame, 2);
        assert_eq!(p.time_until_next(&a), Some(50 * MS));
        p.set_speed(0.25);
        p.advance(399 * MS, &a);
        assert_eq!(p.frame, 2);
        p.advance(MS, &a);
        assert_eq!(p.frame, 3);
        p.set_speed(100.0);
        assert_eq!(p.speed, MAX_SPEED);
        p.set_speed(0.0);
        assert_eq!(p.speed, MIN_SPEED);
    }

    #[test]
    fn reverse_playback_at_double_speed() {
        let a = anim(&[100; 500]);
        let mut p = Player {
            reverse: true,
            ..Default::default()
        };
        p.set_speed(2.0);
        p.seek(499, a.frame_count());
        let mut seen = vec![p.frame];
        for _ in 0..20 {
            if p.advance(50 * MS, &a) {
                seen.push(p.frame);
            }
        }
        // Every frame is visited in order, one per 50 ms of wall time.
        let expected: Vec<usize> = (479..=499).rev().collect();
        assert_eq!(seen, expected);
        // Reverse wraps from the first frame to the last.
        p.seek(0, a.frame_count());
        p.advance(50 * MS, &a);
        assert_eq!(p.frame, 499);
    }

    #[test]
    fn non_looping_stops_at_the_end() {
        let a = anim(&[10, 10, 10]);
        let mut p = Player {
            looping: false,
            ..Default::default()
        };
        p.advance(Duration::from_secs(5), &a);
        assert_eq!(p.frame, 2);
        assert!(!p.playing);
        assert_eq!(p.time_until_next(&a), None);
        let mut p = Player {
            looping: false,
            reverse: true,
            ..Default::default()
        };
        p.seek(2, 3);
        p.advance(Duration::from_secs(5), &a);
        assert_eq!((p.frame, p.playing), (0, false));
    }

    #[test]
    fn stepping_pauses_and_wraps() {
        let a = anim(&[10, 10, 10]);
        let mut p = Player::default();
        p.step(1, a.frame_count());
        assert_eq!((p.frame, p.playing), (1, false));
        p.step(-1, a.frame_count());
        p.step(-1, a.frame_count());
        assert_eq!(p.frame, 2);
        p.step(1, a.frame_count());
        assert_eq!(p.frame, 0);
        assert!(
            !p.advance(Duration::from_secs(1), &a),
            "paused players do not advance"
        );
        p.seek(99, a.frame_count());
        assert_eq!(p.frame, 2);
    }

    #[test]
    fn long_stall_does_not_spin() {
        let a = anim(&[1, 1, 1]);
        let mut p = Player::default();
        let start = std::time::Instant::now();
        p.advance(Duration::from_secs(3600 * 24 * 365), &a);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(p.frame < 3);
    }

    #[test]
    fn single_frame_never_animates() {
        let a = anim(&[100]);
        let mut p = Player::default();
        assert!(!p.advance(Duration::from_secs(1), &a));
        assert_eq!(p.time_until_next(&a), None);
    }

    #[test]
    fn packed_storage_is_frame_accurate() {
        let frames: Vec<RawFrame> = (0..50u8)
            .map(|i| RawFrame {
                image: RgbaImage::from_pixel(32, 32, Rgba([i, 255 - i, 7, 255])),
                delay_ms: 20,
            })
            .collect();
        let raw = AnimatedImage::from_frames(frames.clone(), usize::MAX).unwrap();
        let packed = AnimatedImage::from_frames(frames, 1024).unwrap();
        assert!(!raw.is_packed() && packed.is_packed());
        assert!(packed.bytes() < raw.bytes() / 10);
        for i in [0, 17, 49] {
            assert_eq!(raw.frame(i).unwrap(), packed.frame(i).unwrap());
        }
        assert!(packed.frame(50).is_none());
        assert_eq!(packed.total_duration(), Duration::from_secs(1));
    }

    #[test]
    fn frames_are_premultiplied() {
        let frames = vec![RawFrame {
            image: RgbaImage::from_pixel(1, 1, Rgba([200, 100, 50, 128])),
            delay_ms: 10,
        }];
        let a = AnimatedImage::from_frames(frames, usize::MAX).unwrap();
        assert_eq!(a.frame(0).unwrap().as_slice(), &[100, 50, 25, 128]);
    }
}
