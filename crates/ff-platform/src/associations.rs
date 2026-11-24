//! "Default app" handling for file types.
//!
//! * macOS – Launch Services: query and set the default handler per file type.
//! * Linux – `xdg-mime`: query and set the default `.desktop` entry per MIME type.
//! * Windows – defaults can only be changed by the user in the Settings app
//!   (the per-user choice is protected), so we can only open that page.

use crate::PlatformError;

/// What the OS lets an application do about default apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationSupport {
    /// Defaults can be queried and set per file type.
    PerType,
    /// The user has to choose in the system settings; we can open them.
    SystemSettings,
    /// The app is not installed in a way the OS can register (e.g. run from a
    /// build directory instead of an app bundle / installed `.desktop` entry).
    NotInstalled,
    /// Not available on this system.
    Unavailable,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{AssociationSupport, PlatformError};
    use objc2_core_foundation::{CFRetained, CFString};
    use std::ffi::c_void;
    use std::ptr::NonNull;

    type CFStringRef = *const c_void;
    /// `kLSRolesAll`
    const ROLES_ALL: u32 = 0xFFFF_FFFF;

    // These Launch Services / UTType functions are deprecated in favour of
    // NSWorkspace/UTType APIs but remain the only plain-C way to do this.
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        static kUTTagClassFilenameExtension: CFStringRef;
        fn UTTypeCreatePreferredIdentifierForTag(
            class: CFStringRef,
            tag: CFStringRef,
            conforming: CFStringRef,
        ) -> CFStringRef;
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
        ) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            bundle_id: CFStringRef,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFBundleGetMainBundle() -> *const c_void;
        fn CFBundleGetIdentifier(bundle: *const c_void) -> CFStringRef;
    }

    fn as_ref(s: &CFString) -> CFStringRef {
        (s as *const CFString).cast()
    }

    /// Take ownership of a +1 CFString returned by a "Create"/"Copy" function.
    fn take(raw: CFStringRef) -> Option<CFRetained<CFString>> {
        let ptr = NonNull::new(raw.cast_mut().cast::<CFString>())?;
        // SAFETY: `raw` is a non-null CFString we own one reference to (Create/Copy rule).
        Some(unsafe { CFRetained::from_raw(ptr) })
    }

    /// Bundle identifier of the running app, if it runs from an app bundle.
    fn bundle_id() -> Option<String> {
        // SAFETY: both calls follow the Get rule and accept/return possibly-null pointers;
        // the returned string is only borrowed for the duration of `to_string`.
        unsafe {
            let bundle = CFBundleGetMainBundle();
            if bundle.is_null() {
                return None;
            }
            let id = CFBundleGetIdentifier(bundle);
            if id.is_null() {
                return None;
            }
            Some((*id.cast::<CFString>()).to_string())
        }
    }

    fn uti_for(extension: &str) -> Option<CFRetained<CFString>> {
        let tag = CFString::from_str(extension);
        // SAFETY: the tag class is an immortal CFString constant, `tag` is a valid CFString
        // and a null "conforming to" hint is allowed.
        take(unsafe {
            UTTypeCreatePreferredIdentifierForTag(
                kUTTagClassFilenameExtension,
                as_ref(&tag),
                std::ptr::null(),
            )
        })
    }

    pub fn support() -> AssociationSupport {
        if bundle_id().is_some() {
            AssociationSupport::PerType
        } else {
            AssociationSupport::NotInstalled
        }
    }

    pub fn is_default(extension: &str, _mime: &str) -> Option<bool> {
        let me = bundle_id()?;
        let uti = uti_for(extension)?;
        // SAFETY: `uti` is a valid CFString; the result follows the Copy rule (may be null).
        let handler =
            take(unsafe { LSCopyDefaultRoleHandlerForContentType(as_ref(&uti), ROLES_ALL) });
        Some(
            handler
                .map(|h| h.to_string().eq_ignore_ascii_case(&me))
                .unwrap_or(false),
        )
    }

    pub fn set_default(extension: &str, _mime: &str) -> Result<(), PlatformError> {
        let me = bundle_id()
            .ok_or_else(|| PlatformError::Unsupported("not running from an app bundle".into()))?;
        let uti = uti_for(extension)
            .ok_or_else(|| PlatformError::Decode(format!("no type identifier for .{extension}")))?;
        let me = CFString::from_str(&me);
        // SAFETY: both arguments are valid CFStrings for the duration of the call.
        let status =
            unsafe { LSSetDefaultRoleHandlerForContentType(as_ref(&uti), ROLES_ALL, as_ref(&me)) };
        if status == 0 {
            Ok(())
        } else {
            Err(PlatformError::Decode(format!(
                "Launch Services error {status} for .{extension}"
            )))
        }
    }

    pub fn open_settings() -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported("not needed on macOS".into()))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{AssociationSupport, PlatformError};
    use std::process::{Command, Stdio};

    const DESKTOP_ENTRY: &str = "flickflock.desktop";

    fn xdg_mime(args: &[&str]) -> Option<String> {
        let out = Command::new("xdg-mime")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Is our desktop entry installed where the desktop environment looks for it?
    fn entry_installed() -> bool {
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        if let Some(home) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            dirs.push(home.into());
        } else if let Some(home) = std::env::var_os("HOME") {
            dirs.push(std::path::PathBuf::from(home).join(".local/share"));
        }
        let system =
            std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        dirs.extend(
            system
                .split(':')
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from),
        );
        dirs.iter()
            .any(|d| d.join("applications").join(DESKTOP_ENTRY).exists())
    }

    pub fn support() -> AssociationSupport {
        if xdg_mime(&["--version"]).is_none() {
            AssociationSupport::Unavailable
        } else if entry_installed() {
            AssociationSupport::PerType
        } else {
            AssociationSupport::NotInstalled
        }
    }

    pub fn is_default(_extension: &str, mime: &str) -> Option<bool> {
        Some(xdg_mime(&["query", "default", mime])? == DESKTOP_ENTRY)
    }

    pub fn set_default(_extension: &str, mime: &str) -> Result<(), PlatformError> {
        xdg_mime(&["default", DESKTOP_ENTRY, mime])
            .map(|_| ())
            .ok_or_else(|| {
                PlatformError::Decode(format!("xdg-mime could not set the default for {mime}"))
            })
    }

    pub fn open_settings() -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported("not needed on Linux".into()))
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{AssociationSupport, PlatformError};

    pub fn support() -> AssociationSupport {
        AssociationSupport::SystemSettings
    }

    pub fn is_default(_extension: &str, _mime: &str) -> Option<bool> {
        None
    }

    pub fn set_default(_extension: &str, _mime: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "Windows only lets the user change default apps in Settings".into(),
        ))
    }

    /// Open Settings ▸ Apps ▸ Default apps.
    pub fn open_settings() -> Result<(), PlatformError> {
        std::process::Command::new("explorer.exe")
            .arg("ms-settings:defaultapps")
            .spawn()
            .map(|_| ())
            .map_err(PlatformError::Io)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    use super::{AssociationSupport, PlatformError};

    pub fn support() -> AssociationSupport {
        AssociationSupport::Unavailable
    }
    pub fn is_default(_extension: &str, _mime: &str) -> Option<bool> {
        None
    }
    pub fn set_default(_extension: &str, _mime: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "file associations are not supported on this system".into(),
        ))
    }
    pub fn open_settings() -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "file associations are not supported on this system".into(),
        ))
    }
}

pub(crate) use imp::{is_default, open_settings, set_default, support};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_never_panic() {
        let _ = support();
        for ext in ["jpg", "png", "definitely-not-a-type", ""] {
            let _ = is_default(ext, "image/jpeg");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unbundled_test_binary_is_reported_as_not_installed() {
        // `cargo test` binaries have no bundle identifier.
        assert_eq!(support(), AssociationSupport::NotInstalled);
        assert_eq!(is_default("jpg", "image/jpeg"), None);
        assert!(set_default("jpg", "image/jpeg").is_err());
    }
}
