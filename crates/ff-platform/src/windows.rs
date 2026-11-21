//! Windows: HEIF/HEIC/AVIF through the Windows Imaging Component (WIC).
//!
//! WIC only knows these formats when the matching Microsoft Store extensions are installed;
//! when they are not, the decoder reports [`PlatformError::Unsupported`] with a message that
//! tells the user what to install.
//!
//! Orientation: the HEIF container stores rotation/mirroring as image *transformations*
//! (`irot`/`imir`), and the WIC HEIF codec applies them while decoding, so the pixels arrive
//! upright. Applying the EXIF orientation tag on top would rotate such files twice, so this
//! module deliberately does not read it.

use std::cell::Cell;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    GENERIC_READ, RPC_E_CHANGED_MODE, WINCODEC_ERR_COMPONENTNOTFOUND,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

use crate::{
    check_readable, rgba_len, NoIntegration, PlatformDecoder, PlatformError, PlatformIntegration,
    RawImage,
};

const MISSING_CODEC_MESSAGE: &str = "Windows has no codec installed for this image. Install the \
     free \"HEIF Image Extensions\" from the Microsoft Store \
     (ms-windows-store://pdp/?ProductId=9PMMSR1CGPWG). HEIC photos additionally need the \
     \"HEVC Video Extensions\", and AVIF images the \"AV1 Video Extension\".";

/// `MF_E_TOPO_CODEC_NOT_FOUND`: the HEIF container codec is installed but the video codec that
/// decodes the actual pixels (HEVC or AV1) is not.
const MF_E_TOPO_CODEC_NOT_FOUND: HRESULT = HRESULT(0xC00D5212_u32 as i32);

fn map_error(context: &str, err: windows::core::Error) -> PlatformError {
    let code = err.code();
    if code == WINCODEC_ERR_COMPONENTNOTFOUND || code == MF_E_TOPO_CODEC_NOT_FOUND {
        PlatformError::Unsupported(MISSING_CODEC_MESSAGE.into())
    } else {
        PlatformError::Decode(format!("{context}: {} ({code:?})", err.message()))
    }
}

/// Makes sure COM is usable on the calling thread.
///
/// The decoder is called from long-lived worker threads, so COM is initialised once per thread
/// (multithreaded apartment) and intentionally never uninitialised: `CoUninitialize` must not
/// run from a thread-local destructor (those run under the loader lock on Windows), and the
/// process tears everything down on exit anyway.
fn ensure_com() -> Result<(), PlatformError> {
    thread_local! {
        static READY: Cell<bool> = const { Cell::new(false) };
    }
    READY.with(|ready| {
        if ready.get() {
            return Ok(());
        }
        // SAFETY: plain COM initialisation for the current thread; the reserved pointer is null.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK / S_FALSE: initialised (now or earlier). RPC_E_CHANGED_MODE: somebody already put
        // this thread in a single-threaded apartment, which works just as well for WIC.
        if hr.is_ok() || hr == RPC_E_CHANGED_MODE {
            ready.set(true);
            Ok(())
        } else {
            Err(PlatformError::Decode(format!(
                "could not initialise COM: {}",
                hr.message()
            )))
        }
    })
}

struct WicDecoder;

impl PlatformDecoder for WicDecoder {
    fn name(&self) -> &'static str {
        "WIC"
    }

    fn decode(&self, path: &Path) -> Result<RawImage, PlatformError> {
        check_readable(path)?;
        ensure_com()?;

        // The path is passed through untouched (no canonicalisation), so `\\?\` long paths and
        // UNC paths keep working exactly as the caller spelled them.
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if wide[..wide.len() - 1].contains(&0) {
            return Err(PlatformError::Decode(
                "file path contains a NUL character".into(),
            ));
        }

        // SAFETY: COM is initialised on this thread (`ensure_com`), and the CLSID/interface pair
        // is the documented one for the WIC factory.
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| map_error("could not create the WIC factory", e))?;

        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
        let decoder = unsafe {
            factory.CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
        }
        .map_err(|e| map_error("could not open the image", e))?;

        // SAFETY: `decoder` is a valid COM object; frame 0 is the primary image.
        let frame =
            unsafe { decoder.GetFrame(0) }.map_err(|e| map_error("could not read the image", e))?;

        // SAFETY: `factory` is a valid COM object.
        let converter = unsafe { factory.CreateFormatConverter() }
            .map_err(|e| map_error("could not create a pixel converter", e))?;
        // SAFETY: `frame` and `converter` are valid COM objects, the format GUID lives for the
        // whole call, and a null palette is allowed for non-indexed target formats.
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }
        .map_err(|e| map_error("could not convert the image to RGBA", e))?;

        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: both out-pointers refer to live local variables.
        unsafe { converter.GetSize(&mut width, &mut height) }
            .map_err(|e| map_error("could not read the image size", e))?;

        let len = rgba_len(width as usize, height as usize)?;
        // WIC takes the stride and the buffer size as 32-bit values.
        let stride = width
            .checked_mul(4)
            .filter(|_| u32::try_from(len).is_ok())
            .ok_or_else(|| PlatformError::Decode("image is too large".into()))?;

        let mut rgba = vec![0u8; len];
        // SAFETY: a null rectangle means "the whole image"; `rgba` holds exactly
        // `stride * height` bytes, which is what WIC writes.
        unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut rgba) }
            .map_err(|e| map_error("could not decode the image", e))?;

        Ok(RawImage {
            width,
            height,
            rgba,
        })
    }
}

pub(crate) fn decoder() -> &'static dyn PlatformDecoder {
    &WicDecoder
}

pub(crate) fn integration() -> &'static dyn PlatformIntegration {
    &NoIntegration
}
