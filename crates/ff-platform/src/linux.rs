//! Linux: HEIF/HEIC/AVIF through the system `libheif`, loaded at runtime.
//!
//! `libheif` is optional. When it is not installed the decoder reports
//! [`PlatformError::Unsupported`] with a friendly message instead of failing to start.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use libloading::Library;

use crate::{
    check_readable, rgba_len, NoIntegration, PlatformDecoder, PlatformError, PlatformIntegration,
    RawImage,
};

const MISSING_MESSAGE: &str = "HEIC is not supported on this system (install libheif)";

/// Sonames to try, most specific first. `libheif.so` only exists with the development package.
const LIBRARY_NAMES: &[&str] = &["libheif.so.1", "libheif.so"];

// Values from libheif's `heif.h` (stable since 1.0).
/// `heif_error_Ok`
const HEIF_ERROR_OK: c_int = 0;
/// `heif_error_Unsupported_filetype`
const HEIF_ERROR_UNSUPPORTED_FILETYPE: c_int = 3;
/// `heif_error_Unsupported_feature` (also used for "no decoder plugin for this codec").
const HEIF_ERROR_UNSUPPORTED_FEATURE: c_int = 4;
/// `heif_colorspace_RGB`
const HEIF_COLORSPACE_RGB: c_int = 1;
/// `heif_chroma_interleaved_RGBA` (8 bits per channel, straight alpha)
const HEIF_CHROMA_INTERLEAVED_RGBA: c_int = 11;
/// `heif_channel_interleaved`
const HEIF_CHANNEL_INTERLEAVED: c_int = 10;

/// `struct heif_error`, returned by value from most libheif calls.
#[repr(C)]
#[derive(Clone, Copy)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

type HeifContext = c_void;
type HeifImageHandle = c_void;
type HeifImage = c_void;

/// The subset of the libheif C API that we use, resolved once.
struct LibHeif {
    context_alloc: unsafe extern "C" fn() -> *mut HeifContext,
    context_free: unsafe extern "C" fn(*mut HeifContext),
    context_read_from_file:
        unsafe extern "C" fn(*mut HeifContext, *const c_char, *const c_void) -> HeifError,
    context_get_primary_image_handle:
        unsafe extern "C" fn(*mut HeifContext, *mut *mut HeifImageHandle) -> HeifError,
    image_handle_release: unsafe extern "C" fn(*const HeifImageHandle),
    decode_image: unsafe extern "C" fn(
        *const HeifImageHandle,
        *mut *mut HeifImage,
        c_int,
        c_int,
        *const c_void,
    ) -> HeifError,
    image_release: unsafe extern "C" fn(*const HeifImage),
    image_get_width: unsafe extern "C" fn(*const HeifImage, c_int) -> c_int,
    image_get_height: unsafe extern "C" fn(*const HeifImage, c_int) -> c_int,
    image_get_plane_readonly:
        unsafe extern "C" fn(*const HeifImage, c_int, *mut c_int) -> *const u8,
    /// Keeps the shared object mapped for as long as the function pointers above are used.
    _library: Library,
}

impl LibHeif {
    fn load() -> Option<Self> {
        for name in LIBRARY_NAMES {
            // SAFETY: loading libheif runs its initialisers, which have no preconditions; we
            // trust the system library the same way a normally linked program would.
            let library = match unsafe { Library::new(name) } {
                Ok(library) => library,
                Err(err) => {
                    tracing::debug!("could not load {name}: {err}");
                    continue;
                }
            };
            match Self::resolve(library) {
                Ok(lib) => {
                    tracing::debug!("using {name} for HEIF decoding");
                    return Some(lib);
                }
                Err(err) => tracing::warn!("{name} is missing expected symbols: {err}"),
            }
        }
        None
    }

    fn resolve(library: Library) -> Result<Self, libloading::Error> {
        macro_rules! symbol {
            ($name:literal) => {
                // SAFETY: the symbol is a libheif C function whose signature matches the type of
                // the field it is assigned to (checked against `heif.h`). The copied function
                // pointer stays valid because `_library` is stored alongside it.
                *unsafe { library.get(concat!($name, "\0").as_bytes()) }?
            };
        }
        Ok(Self {
            context_alloc: symbol!("heif_context_alloc"),
            context_free: symbol!("heif_context_free"),
            context_read_from_file: symbol!("heif_context_read_from_file"),
            context_get_primary_image_handle: symbol!("heif_context_get_primary_image_handle"),
            image_handle_release: symbol!("heif_image_handle_release"),
            decode_image: symbol!("heif_decode_image"),
            image_release: symbol!("heif_image_release"),
            image_get_width: symbol!("heif_image_get_width"),
            image_get_height: symbol!("heif_image_get_height"),
            image_get_plane_readonly: symbol!("heif_image_get_plane_readonly"),
            _library: library,
        })
    }

    /// Converts a `heif_error` into a `Result`.
    fn check(&self, err: HeifError) -> Result<(), PlatformError> {
        if err.code == HEIF_ERROR_OK {
            return Ok(());
        }
        let detail = if err.message.is_null() {
            format!("libheif error {}.{}", err.code, err.subcode)
        } else {
            // SAFETY: libheif guarantees `message` is a NUL-terminated static string when set.
            unsafe { CStr::from_ptr(err.message) }
                .to_string_lossy()
                .into_owned()
        };
        Err(match err.code {
            HEIF_ERROR_UNSUPPORTED_FILETYPE | HEIF_ERROR_UNSUPPORTED_FEATURE => {
                PlatformError::Unsupported(format!(
                    "The installed libheif cannot decode this image (a codec plugin may be \
                     missing): {detail}"
                ))
            }
            _ => PlatformError::Decode(detail),
        })
    }
}

fn lib() -> Option<&'static LibHeif> {
    static LIB: OnceLock<Option<LibHeif>> = OnceLock::new();
    LIB.get_or_init(LibHeif::load).as_ref()
}

/// Owns a `heif_context*`.
struct Context<'a>(&'a LibHeif, *mut HeifContext);
impl Drop for Context<'_> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `heif_context_alloc` and is freed exactly once.
        unsafe { (self.0.context_free)(self.1) }
    }
}

/// Owns a `heif_image_handle*`.
struct Handle<'a>(&'a LibHeif, *mut HeifImageHandle);
impl Drop for Handle<'_> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from libheif and is released exactly once.
        unsafe { (self.0.image_handle_release)(self.1) }
    }
}

/// Owns a decoded `heif_image*`.
struct Image<'a>(&'a LibHeif, *mut HeifImage);
impl Drop for Image<'_> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `heif_decode_image` and is released exactly once.
        unsafe { (self.0.image_release)(self.1) }
    }
}

struct LibHeifDecoder;

impl PlatformDecoder for LibHeifDecoder {
    fn name(&self) -> &'static str {
        if lib().is_some() {
            "libheif"
        } else {
            "none"
        }
    }

    fn decode(&self, path: &Path) -> Result<RawImage, PlatformError> {
        let lib = lib().ok_or_else(|| PlatformError::Unsupported(MISSING_MESSAGE.into()))?;
        check_readable(path)?;
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| PlatformError::Decode("file path contains a NUL byte".into()))?;

        // SAFETY: no preconditions; returns null on allocation failure.
        let ctx = unsafe { (lib.context_alloc)() };
        if ctx.is_null() {
            return Err(PlatformError::Decode(
                "libheif could not allocate a context".into(),
            ));
        }
        let ctx = Context(lib, ctx);

        // SAFETY: `ctx` is a live context, `c_path` is NUL-terminated, and the reading-options
        // pointer is documented as "should be NULL".
        lib.check(unsafe { (lib.context_read_from_file)(ctx.1, c_path.as_ptr(), ptr::null()) })?;

        let mut handle = ptr::null_mut();
        // SAFETY: `ctx` is a live context and `handle` is a valid out-pointer.
        lib.check(unsafe { (lib.context_get_primary_image_handle)(ctx.1, &mut handle) })?;
        if handle.is_null() {
            return Err(PlatformError::Decode("file has no primary image".into()));
        }
        let handle = Handle(lib, handle);

        // Null decoding options means defaults, which apply the container transformations
        // (rotation, mirroring, cropping), so no separate orientation handling is needed.
        let mut image = ptr::null_mut();
        // SAFETY: `handle` is a live image handle, `image` is a valid out-pointer and null
        // options are allowed.
        lib.check(unsafe {
            (lib.decode_image)(
                handle.1,
                &mut image,
                HEIF_COLORSPACE_RGB,
                HEIF_CHROMA_INTERLEAVED_RGBA,
                ptr::null(),
            )
        })?;
        if image.is_null() {
            return Err(PlatformError::Decode("libheif returned no image".into()));
        }
        let image = Image(lib, image);

        // SAFETY: `image` is a live decoded image.
        let (width, height) = unsafe {
            (
                (lib.image_get_width)(image.1, HEIF_CHANNEL_INTERLEAVED),
                (lib.image_get_height)(image.1, HEIF_CHANNEL_INTERLEAVED),
            )
        };
        let invalid = || PlatformError::Decode("libheif returned an invalid image".into());
        let width = usize::try_from(width).map_err(|_| invalid())?;
        let height = usize::try_from(height).map_err(|_| invalid())?;
        let len = rgba_len(width, height)?;
        let row_len = width * 4;

        let mut stride: c_int = 0;
        // SAFETY: `image` is a live decoded image and `stride` is a valid out-pointer.
        let plane = unsafe {
            (lib.image_get_plane_readonly)(image.1, HEIF_CHANNEL_INTERLEAVED, &mut stride)
        };
        let stride = usize::try_from(stride).map_err(|_| invalid())?;
        if plane.is_null() || stride < row_len {
            return Err(invalid());
        }

        let mut rgba = Vec::with_capacity(len);
        for y in 0..height {
            // SAFETY: the interleaved plane holds `height` rows of `stride` bytes each, of which
            // the first `width * 4` are pixel data; `image` keeps it alive during the copy.
            let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_len) };
            rgba.extend_from_slice(row);
        }

        Ok(RawImage {
            width: width as u32,
            height: height as u32,
            rgba,
        })
    }
}

pub(crate) fn decoder() -> &'static dyn PlatformDecoder {
    &LibHeifDecoder
}

pub(crate) fn integration() -> &'static dyn PlatformIntegration {
    &NoIntegration
}
