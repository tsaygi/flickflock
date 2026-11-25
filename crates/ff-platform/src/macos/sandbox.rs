//! Folder access on macOS: remembered grants (security-scoped bookmarks) and Finder/default
//! app hand-off through `NSWorkspace`, because a sandboxed app cannot spawn `/usr/bin/open`.
//!
//! The sandbox only grants the exact file the user opened, while FlickFlock browses the whole
//! folder, so the folder is requested once and the grant is kept in `NSUserDefaults`.
//!
//! Outside the sandbox the same mechanism is the way out when the privacy system refuses a
//! folder (iCloud Drive, Desktop, Documents, … after "Don't Allow", or when no prompt is
//! offered at all): a folder picked in the system dialog is readable regardless.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{msg_send, MainThreadMarker};
use objc2_foundation::{NSString, NSURL};

const BOOKMARKS_KEY: &str = "FolderBookmarks";
/// Each active scope holds a kernel resource, so only the most recent grants are kept.
const MAX_BOOKMARKS: usize = 64;
/// `NSURLBookmarkCreationWithSecurityScope`.
const BOOKMARK_CREATE_SCOPED: usize = 1 << 11;
/// `NSURLBookmarkResolutionWithoutUI | NSURLBookmarkResolutionWithSecurityScope`.
const BOOKMARK_RESOLVE_SCOPED: usize = (1 << 8) | (1 << 10);
/// `NSModalResponseOK`.
const MODAL_OK: isize = 1;

/// Folders the user declined this session, so browsing does not ask again for each file.
static DECLINED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Is this process running inside the App Sandbox?
pub fn is_sandboxed() -> bool {
    std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some()
}

fn class(name: &std::ffi::CStr) -> Option<&'static AnyClass> {
    AnyClass::get(name)
}

fn defaults() -> Option<Retained<AnyObject>> {
    let class = class(c"NSUserDefaults")?;
    // SAFETY: `+standardUserDefaults` takes no arguments and returns a shared object.
    unsafe { msg_send![class, standardUserDefaults] }
}

fn stored_bookmarks() -> Vec<Retained<AnyObject>> {
    let Some(defaults) = defaults() else {
        return Vec::new();
    };
    let key = NSString::from_str(BOOKMARKS_KEY);
    // SAFETY: `arrayForKey:` takes an NSString and returns an NSArray or nil.
    let array: Option<Retained<AnyObject>> = unsafe { msg_send![&*defaults, arrayForKey: &*key] };
    let Some(array) = array else {
        return Vec::new();
    };
    // SAFETY: `array` is an NSArray and every index is below its count.
    unsafe {
        let count: usize = msg_send![&*array, count];
        (0..count)
            .filter_map(|i| msg_send![&*array, objectAtIndex: i])
            .collect()
    }
}

fn store_bookmarks(bookmarks: &[Retained<AnyObject>]) {
    let (Some(defaults), Some(class)) = (defaults(), class(c"NSMutableArray")) else {
        return;
    };
    let start = bookmarks.len().saturating_sub(MAX_BOOKMARKS);
    // SAFETY: `+array` returns a new mutable array and `addObject:` receives non-nil objects.
    unsafe {
        let array: Option<Retained<AnyObject>> = msg_send![class, array];
        let Some(array) = array else {
            return;
        };
        for bookmark in &bookmarks[start..] {
            let _: () = msg_send![&*array, addObject: &**bookmark];
        }
        let key = NSString::from_str(BOOKMARKS_KEY);
        let _: () = msg_send![&*defaults, setObject: &*array, forKey: &*key];
    }
}

fn create_bookmark(url: &NSURL) -> Option<Retained<AnyObject>> {
    // SAFETY: nil keys, relative URL and error pointer are all documented as allowed.
    unsafe {
        msg_send![
            url,
            bookmarkDataWithOptions: BOOKMARK_CREATE_SCOPED,
            includingResourceValuesForKeys: Option::<&AnyObject>::None,
            relativeToURL: Option::<&AnyObject>::None,
            error: std::ptr::null_mut::<*mut AnyObject>()
        ]
    }
}

/// Resolves a bookmark and starts its scope, which stays active for the life of the process.
/// Returns the bookmark to keep (refreshed when the folder moved), or `None` when it is dead.
fn activate(bookmark: Retained<AnyObject>) -> Option<Retained<AnyObject>> {
    let class = class(c"NSURL")?;
    let mut stale = Bool::NO;
    // SAFETY: `bookmark` is NSData, `stale` outlives the call, and nil relative URL and error
    // pointer are allowed.
    let url: Option<Retained<NSURL>> = unsafe {
        msg_send![
            class,
            URLByResolvingBookmarkData: &*bookmark,
            options: BOOKMARK_RESOLVE_SCOPED,
            relativeToURL: Option::<&AnyObject>::None,
            bookmarkDataIsStale: &mut stale,
            error: std::ptr::null_mut::<*mut AnyObject>()
        ]
    };
    let url = url?;
    // SAFETY: takes no arguments and returns BOOL.
    let started: bool = unsafe { msg_send![&*url, startAccessingSecurityScopedResource] };
    if !started {
        return None;
    }
    if stale.as_bool() {
        return Some(create_bookmark(&url).unwrap_or(bookmark));
    }
    Some(bookmark)
}

/// Re-activates every remembered folder. Call once at startup.
pub fn restore_folder_access() {
    let live: Vec<_> = stored_bookmarks()
        .into_iter()
        .filter_map(activate)
        .collect();
    store_bookmarks(&live);
}

/// Keeps access to `dir`, which the user just picked in a system dialog, across launches.
pub fn remember_folder(dir: &Path) {
    let Some(url) = NSURL::from_directory_path(dir) else {
        return;
    };
    let Some(bookmark) = create_bookmark(&url) else {
        tracing::warn!("could not create a bookmark for {}", dir.display());
        return;
    };
    let Some(bookmark) = activate(bookmark) else {
        return;
    };
    let mut bookmarks = stored_bookmarks();
    bookmarks.push(bookmark);
    store_bookmarks(&bookmarks);
}

/// Makes sure `dir` can be listed, asking the user once when it cannot. The dialog starts at
/// `suggested`, normally one level up so that neighbouring folders are covered too. `message`
/// and `prompt` are its already translated texts.
///
/// Must be called on the main thread. Always `true` outside the sandbox.
pub fn ensure_folder_access(dir: &Path, suggested: &Path, message: &str, prompt: &str) -> bool {
    if !is_sandboxed() || std::fs::read_dir(dir).is_ok() {
        return true;
    }
    if DECLINED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|d| d == dir)
    {
        return false;
    }
    if MainThreadMarker::new().is_none() {
        return false;
    }
    if let Some(chosen) = ask_for_folder(suggested, message, prompt) {
        remember_folder(&chosen);
    }
    let granted = std::fs::read_dir(dir).is_ok();
    if !granted {
        DECLINED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(dir.to_path_buf());
    }
    granted
}

/// Asks the user to pick `dir` (or a folder above it) in the system dialog, which makes it
/// readable whatever the privacy settings say, and remembers the grant. The dialog starts at
/// `suggested`. Returns whether the chosen folder covers `dir`.
///
/// Unlike [`ensure_folder_access`] this never touches the folder itself, so it cannot block on
/// a privacy prompt. Must be called on the main thread.
pub fn request_folder_access(dir: &Path, suggested: &Path, message: &str, prompt: &str) -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    let Some(chosen) = ask_for_folder(suggested, message, prompt) else {
        return false;
    };
    remember_folder(&chosen);
    dir.starts_with(&chosen)
}

fn ask_for_folder(suggested: &Path, message: &str, prompt: &str) -> Option<PathBuf> {
    let class = class(c"NSOpenPanel")?;
    let message = NSString::from_str(message);
    let prompt = NSString::from_str(prompt);
    let start = NSURL::from_directory_path(suggested);
    // SAFETY: called on the main thread (checked by the caller); every selector below is a
    // documented NSOpenPanel/NSSavePanel method with these argument types.
    unsafe {
        let panel: Option<Retained<AnyObject>> = msg_send![class, openPanel];
        let panel = panel?;
        let _: () = msg_send![&*panel, setCanChooseDirectories: true];
        let _: () = msg_send![&*panel, setCanChooseFiles: false];
        let _: () = msg_send![&*panel, setAllowsMultipleSelection: false];
        let _: () = msg_send![&*panel, setMessage: &*message];
        let _: () = msg_send![&*panel, setPrompt: &*prompt];
        if let Some(start) = &start {
            let _: () = msg_send![&*panel, setDirectoryURL: &**start];
        }
        let response: isize = msg_send![&*panel, runModal];
        if response != MODAL_OK {
            return None;
        }
        let url: Option<Retained<NSURL>> = msg_send![&*panel, URL];
        url?.to_file_path()
    }
}

fn workspace() -> std::io::Result<Retained<AnyObject>> {
    let unavailable = || std::io::Error::other("NSWorkspace is unavailable");
    let class = class(c"NSWorkspace").ok_or_else(unavailable)?;
    // SAFETY: `+sharedWorkspace` takes no arguments and returns a shared object.
    let workspace: Option<Retained<AnyObject>> = unsafe { msg_send![class, sharedWorkspace] };
    workspace.ok_or_else(unavailable)
}

fn file_url(path: &Path) -> std::io::Result<Retained<NSURL>> {
    NSURL::from_file_path(path)
        .ok_or_else(|| std::io::Error::other("file path cannot be used as a file URL"))
}

/// Selects `path` in a Finder window.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    let workspace = workspace()?;
    let url = file_url(path)?;
    let urls = objc2_foundation::NSArray::from_retained_slice(&[url]);
    // SAFETY: takes an NSArray of NSURL and returns nothing.
    let _: () = unsafe { msg_send![&*workspace, activateFileViewerSelectingURLs: &*urls] };
    Ok(())
}

/// Opens `path` in its default application.
pub fn open(path: &Path) -> std::io::Result<()> {
    let workspace = workspace()?;
    let url = file_url(path)?;
    // SAFETY: takes an NSURL and returns BOOL.
    let opened: bool = unsafe { msg_send![&*workspace, openURL: &*url] };
    if opened {
        Ok(())
    } else {
        Err(std::io::Error::other("no application could open this file"))
    }
}
