//! Thin per-OS layer for FlickFlock.
//!
//! Everything that needs to talk to the operating system directly lives here, behind two small
//! traits:
//!
//! * [`PlatformDecoder`] decodes HEIF/HEIC (and AVIF where the OS can) with the codecs that ship
//!   with the OS: ImageIO on macOS, WIC on Windows, and `libheif` (loaded at runtime, optional)
//!   on Linux.
//! * [`PlatformIntegration`] hooks desktop-shell features, currently the macOS "open file" Apple
//!   Event that Finder sends instead of passing paths on the command line.
//!
//! This crate deliberately has no dependency on the rest of FlickFlock or on any GUI crate.

use std::path::{Path, PathBuf};

mod associations;
pub use associations::AssociationSupport;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as imp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod fallback;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
use fallback as imp;

/// Straight-alpha RGBA8 pixels, already converted to sRGB and with EXIF/container orientation
/// already applied.
#[derive(Clone, PartialEq, Eq)]
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no padding between rows.
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for RawImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_len", &self.rgba.len())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The OS has no codec for this file. The string is a friendly, user-facing message.
    #[error("{0}")]
    Unsupported(String),
    /// The OS codec exists but could not decode the file (corrupt, truncated, too large, ...).
    #[error("{0}")]
    Decode(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Access to the image codecs that ship with the operating system.
pub trait PlatformDecoder: Send + Sync {
    /// Short name of the backend: `"ImageIO"`, `"WIC"`, `"libheif"` or `"none"`.
    fn name(&self) -> &'static str;

    /// Decode HEIF/HEIC (and AVIF where the OS can) from a file path.
    ///
    /// Blocking; intended to be called on worker threads.
    fn decode(&self, path: &Path) -> Result<RawImage, PlatformError>;

    /// Like [`decode`](Self::decode), but may return a smaller image whose longest side is
    /// `>= max_dim` when the OS can do that cheaply. Images that are already smaller than
    /// `max_dim` are returned at their native size (never upscaled).
    ///
    /// The default implementation simply calls [`decode`](Self::decode).
    fn decode_scaled(&self, path: &Path, max_dim: u32) -> Result<RawImage, PlatformError> {
        let _ = max_dim;
        self.decode(path)
    }
}

/// The decoder for the OS this binary was built for.
pub fn decoder() -> &'static dyn PlatformDecoder {
    imp::decoder()
}

/// Desktop-shell integration points.
pub trait PlatformIntegration: Send + Sync {
    /// macOS: make Finder "open file" Apple Events (`application:openURLs:`) call `callback`
    /// with the file paths. The callback runs on the main thread.
    ///
    /// Must be called on the main thread AFTER the winit `EventLoop` has been built and BEFORE
    /// it starts running (so files passed at launch are delivered). Calling it again replaces
    /// the previous callback.
    ///
    /// No-op on other OSes.
    fn install_open_handler(&self, callback: Box<dyn Fn(Vec<PathBuf>) + Send + Sync + 'static>);

    /// What this OS lets the app do about "default app" file associations.
    fn association_support(&self) -> AssociationSupport {
        associations::support()
    }

    /// Is this app the default for files with `extension` (MIME type `mime`)?
    /// `None` when the OS cannot tell. May block briefly; call off the UI thread.
    fn is_default_for(&self, extension: &str, mime: &str) -> Option<bool> {
        associations::is_default(extension, mime)
    }

    /// Make this app the default for files with `extension` (MIME type `mime`).
    fn set_default_for(&self, extension: &str, mime: &str) -> Result<(), PlatformError> {
        associations::set_default(extension, mime)
    }

    /// Open the OS page where the user chooses default apps
    /// (only meaningful for [`AssociationSupport::SystemSettings`]).
    fn open_default_apps_settings(&self) -> Result<(), PlatformError> {
        associations::open_settings()
    }
}

/// The shell integration for the OS this binary was built for.
pub fn integration() -> &'static dyn PlatformIntegration {
    imp::integration()
}

#[cfg(target_os = "macos")]
pub use macos::sandbox::{
    ensure_folder_access, is_sandboxed, open, remember_folder, restore_folder_access, reveal,
};

/// Only macOS has an app sandbox to work with.
#[cfg(not(target_os = "macos"))]
pub fn is_sandboxed() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn restore_folder_access() {}

#[cfg(not(target_os = "macos"))]
pub fn remember_folder(_dir: &Path) {}

#[cfg(not(target_os = "macos"))]
pub fn ensure_folder_access(_dir: &Path, _suggested: &Path, _message: &str, _prompt: &str) -> bool {
    true
}

/// Integration used on every OS that has nothing to hook.
#[cfg(not(target_os = "macos"))]
pub(crate) struct NoIntegration;

#[cfg(not(target_os = "macos"))]
impl PlatformIntegration for NoIntegration {
    fn install_open_handler(&self, _callback: Box<dyn Fn(Vec<PathBuf>) + Send + Sync + 'static>) {}
}

/// Checks that `width * height * 4` is a sane allocation size and returns it in bytes.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) fn rgba_len(width: usize, height: usize) -> Result<usize, PlatformError> {
    if width == 0 || height == 0 {
        return Err(PlatformError::Decode("image has no pixels".into()));
    }
    if u32::try_from(width).is_err() || u32::try_from(height).is_err() {
        return Err(PlatformError::Decode("image is too large".into()));
    }
    width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .filter(|&n| isize::try_from(n).is_ok())
        .ok_or_else(|| PlatformError::Decode("image is too large".into()))
}

/// Turns a failure to even open `path` into an [`PlatformError::Io`] where that is the real cause,
/// so callers get "No such file or directory" instead of an opaque codec error.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) fn check_readable(path: &Path) -> Result<(), PlatformError> {
    std::fs::File::open(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_len_rejects_degenerate_sizes() {
        assert_eq!(rgba_len(2, 3).unwrap(), 24);
        assert!(rgba_len(0, 3).is_err());
        assert!(rgba_len(3, 0).is_err());
        assert!(rgba_len(usize::MAX, 2).is_err());
        assert!(rgba_len(u32::MAX as usize + 1, 1).is_err());
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let path = Path::new("/definitely/not/here/ff-platform-missing.heic");
        assert!(decoder().decode(path).is_err());
        assert!(decoder().decode_scaled(path, 256).is_err());
    }

    #[test]
    fn garbage_file_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("ff-platform-garbage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("garbage.heic");
        std::fs::write(&path, b"this is not an image at all").unwrap();
        assert!(decoder().decode(&path).is_err());
        assert!(decoder().decode_scaled(&path, 64).is_err());
        std::fs::write(&path, b"").unwrap();
        assert!(decoder().decode(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decoder_has_a_name() {
        assert!(["ImageIO", "WIC", "libheif", "none"].contains(&decoder().name()));
    }

    #[test]
    fn open_handler_can_be_requested_off_the_main_thread_without_panicking() {
        // Tests run on worker threads: on macOS this must refuse politely, elsewhere it is a no-op.
        integration().install_open_handler(Box::new(|_| {}));
    }
}
