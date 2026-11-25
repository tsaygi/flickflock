//! macOS: HEIF/HEIC/AVIF through ImageIO, plus the Finder "open file" Apple Event hook.

use std::ffi::{c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Imp, NSObject, Sel};
use objc2::{msg_send, sel, ClassType, MainThreadMarker};
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFString, CFType, CGPoint, CGRect, CGSize, CFURL,
};
use objc2_core_graphics::{
    kCGColorSpaceSRGB, CGBitmapContextCreate, CGBlendMode, CGColorSpace, CGContext, CGImage,
    CGImageAlphaInfo,
};
use objc2_foundation::{NSArray, NSURL};
use objc2_image_io::{
    kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
    kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceThumbnailMaxPixelSize, CGImageSource,
};

use crate::{
    check_readable, rgba_len, PlatformDecoder, PlatformError, PlatformIntegration, RawImage,
};

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

struct ImageIoDecoder;

impl PlatformDecoder for ImageIoDecoder {
    fn name(&self) -> &'static str {
        "ImageIO"
    }

    fn decode(&self, path: &Path) -> Result<RawImage, PlatformError> {
        decode(path, None)
    }

    fn decode_scaled(&self, path: &Path, max_dim: u32) -> Result<RawImage, PlatformError> {
        decode(path, (max_dim > 0).then_some(max_dim))
    }
}

pub(crate) fn decoder() -> &'static dyn PlatformDecoder {
    &ImageIoDecoder
}

/// Decodes `path`. With `max_dim` the longest side of the result is at most `max_dim`
/// (ImageIO never upscales); without it the image is decoded at full size.
///
/// Both paths go through ImageIO's thumbnail API with "create from the full image, always" and
/// "apply the transform", because that is the one ImageIO entry point that bakes the
/// EXIF/container orientation into the pixels for us.
fn decode(path: &Path, max_dim: Option<u32>) -> Result<RawImage, PlatformError> {
    check_readable(path)?;

    let url = CFURL::from_file_path(path)
        .ok_or_else(|| PlatformError::Decode("file path cannot be used as a file URL".into()))?;
    // SAFETY: `url` is a valid CFURL and no options dictionary is passed.
    let source = unsafe { CGImageSource::with_url(&url, None) }
        .ok_or_else(|| PlatformError::Decode("the file could not be opened as an image".into()))?;
    // SAFETY: `source` is a valid image source.
    let count = unsafe { source.count() };
    if count == 0 {
        return Err(PlatformError::Decode(
            "the file is not an image that macOS can decode".into(),
        ));
    }
    // HEIF containers can hold several images (bursts, depth maps, ...); show the primary one.
    // SAFETY: `source` is a valid image source.
    let index = unsafe { source.primary_image_index() }.min(count - 1);

    // For a full-size decode ask for "at most the native longest side", i.e. no scaling.
    let max_pixel_size = match max_dim {
        Some(max_dim) => Some(i64::from(max_dim)),
        None => native_longest_side(&source, index),
    };

    let yes: &CFType = CFBoolean::new(true).as_ref();
    // SAFETY: these ImageIO statics are valid, immortal CFString constants.
    let (key_always, key_transform, key_max) = unsafe {
        (
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceThumbnailMaxPixelSize,
        )
    };
    let max_number = max_pixel_size.map(CFNumber::new_i64);
    let mut keys: Vec<&CFString> = vec![key_always, key_transform];
    let mut values: Vec<&CFType> = vec![yes, yes];
    if let Some(number) = &max_number {
        keys.push(key_max);
        values.push(number.as_ref());
    }
    let options = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);

    // SAFETY: `index` is in range and `options` maps the documented CFString keys to values of
    // the documented types (CFBoolean, CFNumber).
    let image = unsafe { source.thumbnail_at_index(index, Some(options.as_opaque())) }
        .ok_or_else(|| PlatformError::Decode("macOS could not decode this image".into()))?;

    render_to_rgba(&image)
}

/// The longest side of the image as stored (before orientation), from the container metadata.
fn native_longest_side(source: &CGImageSource, index: usize) -> Option<i64> {
    // SAFETY: `source` is valid, `index` is in range and no options dictionary is passed.
    let properties = unsafe { source.properties_at_index(index, None) }?;
    // SAFETY: ImageIO property dictionaries have CFString keys and CF object values.
    let properties: &CFDictionary<CFString, CFType> = unsafe { properties.cast_unchecked() };
    let read = |key: &CFString| -> Option<i64> {
        let value = properties.get(key)?;
        value
            .downcast_ref::<CFNumber>()?
            .as_i64()
            .filter(|&n| n > 0)
    };
    // SAFETY: these ImageIO statics are valid, immortal CFString constants.
    let (key_width, key_height) =
        unsafe { (kCGImagePropertyPixelWidth, kCGImagePropertyPixelHeight) };
    Some(read(key_width)?.max(read(key_height)?))
}

/// Draws `image` into an sRGB RGBA8 bitmap (colour-managed by CoreGraphics) and returns the
/// pixels with straight alpha.
fn render_to_rgba(image: &CGImage) -> Result<RawImage, PlatformError> {
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    let len = rgba_len(width, height)?;

    let alpha_info = CGImage::alpha_info(Some(image));
    let opaque = matches!(
        alpha_info,
        CGImageAlphaInfo::None | CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast
    );
    // With the default (big-endian) byte order "alpha last" means R, G, B, A in memory.
    // CoreGraphics bitmap contexts cannot hold straight alpha, hence premultiplied.
    let bitmap_info = if opaque {
        CGImageAlphaInfo::NoneSkipLast
    } else {
        CGImageAlphaInfo::PremultipliedLast
    };

    // SAFETY: `kCGColorSpaceSRGB` is a valid, immortal CFString constant.
    let color_space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| PlatformError::Decode("could not create the sRGB colour space".into()))?;

    let mut rgba = vec![0u8; len];
    {
        // SAFETY: `rgba` is `height` rows of `width * 4` bytes, matching the geometry passed
        // here, and it outlives `context`, which is dropped at the end of this block before
        // `rgba` is touched again.
        let context = unsafe {
            CGBitmapContextCreate(
                rgba.as_mut_ptr().cast::<c_void>(),
                width,
                height,
                8,
                width * 4,
                Some(&color_space),
                bitmap_info.0,
            )
        }
        .ok_or_else(|| PlatformError::Decode("could not create a bitmap to decode into".into()))?;

        // The destination is empty, so copy instead of blending.
        CGContext::set_blend_mode(Some(&context), CGBlendMode::Copy);
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: width as f64,
                height: height as f64,
            },
        };
        CGContext::draw_image(Some(&context), rect, Some(image));
    }

    if opaque {
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    } else {
        unpremultiply(&mut rgba);
    }

    Ok(RawImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// Converts premultiplied RGBA8 to straight alpha in place.
fn unpremultiply(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// "Open file" Apple Events
// ---------------------------------------------------------------------------------------------

type OpenCallback = dyn Fn(Vec<PathBuf>) + Send + Sync + 'static;

/// The callback that `application:openURLs:` forwards to.
static OPEN_CALLBACK: Mutex<Option<Arc<OpenCallback>>> = Mutex::new(None);

/// Name of the delegate class that is created when the application has no delegate yet.
const FALLBACK_DELEGATE_CLASS: &CStr = c"FFPlatformOpenFilesDelegate";

/// Objective-C type encoding of `- (void)application:(NSApplication *)app openURLs:(NSArray *)urls`.
const OPEN_URLS_ENCODING: &CStr = c"v@:@@";

// Make sure AppKit is loaded even if the final binary links no GUI crate.
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

struct MacIntegration;

impl PlatformIntegration for MacIntegration {
    fn install_open_handler(&self, callback: Box<dyn Fn(Vec<PathBuf>) + Send + Sync + 'static>) {
        if MainThreadMarker::new().is_none() {
            tracing::error!("install_open_handler must be called on the main thread; ignoring");
            return;
        }
        *OPEN_CALLBACK.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::from(callback));
        if let Err(reason) = hook_application_delegate() {
            tracing::error!("could not install the open-file handler: {reason}");
        }
    }
}

pub(crate) fn integration() -> &'static dyn PlatformIntegration {
    &MacIntegration
}

/// Signature of the `application:openURLs:` implementation (receiver, selector, application, URLs).
type OpenUrlsImp = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *const NSArray<NSURL>);

/// `-[<delegate> application:openURLs:]`.
extern "C-unwind" fn application_open_urls(
    _this: *mut AnyObject,
    _cmd: Sel,
    _application: *mut AnyObject,
    urls: *const NSArray<NSURL>,
) {
    // SAFETY: AppKit passes a valid `NSArray<NSURL *> *` (or nil) that stays alive for the
    // duration of this call.
    let Some(urls) = (unsafe { urls.as_ref() }) else {
        return;
    };
    let paths: Vec<PathBuf> = urls
        .to_vec()
        .iter()
        .filter_map(|url| url.to_file_path())
        .collect();
    if paths.is_empty() {
        return;
    }
    let callback = OPEN_CALLBACK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(callback) = callback {
        // A panic must not unwind into AppKit's event dispatch.
        if catch_unwind(AssertUnwindSafe(|| callback(paths))).is_err() {
            tracing::error!("the open-file callback panicked");
        }
    }
}

/// Makes the application delegate respond to `application:openURLs:`.
///
/// The windowing library installs its own `NSApplicationDelegate` when it creates its event
/// loop, and that delegate does not implement `application:openURLs:`. Replacing the delegate
/// would break the windowing library, so the method is added to the delegate's class at runtime
/// instead. If there is no delegate at all, a minimal one is installed.
///
/// Must be called on the main thread.
fn hook_application_delegate() -> Result<(), String> {
    let selector = sel!(application:openURLs:);
    let app_class = AnyClass::get(c"NSApplication").ok_or("AppKit is not loaded")?;
    // SAFETY: `+[NSApplication sharedApplication]` takes no arguments and returns the (never
    // deallocated) application object; we are on the main thread.
    let app: Option<Retained<AnyObject>> = unsafe { msg_send![app_class, sharedApplication] };
    let app = app.ok_or("there is no NSApplication")?;
    // SAFETY: `-[NSApplication delegate]` takes no arguments and returns an object or nil.
    let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![&*app, delegate] };

    let delegate = match delegate {
        Some(delegate) => delegate,
        None => {
            let class = fallback_delegate_class()?;
            // SAFETY: `class` is a plain NSObject subclass without instance variables, so
            // `+new` returns a fully initialised object.
            let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
            let delegate = delegate.ok_or("could not create the application delegate")?;
            // NSApplication does not retain its delegate; ours has to live as long as the app.
            std::mem::forget(delegate.clone());
            delegate
        }
    };

    let class: &AnyClass = delegate.class();
    if class.responds_to(selector) {
        if class.name() == FALLBACK_DELEGATE_CLASS {
            // Our own delegate from an earlier call: the callback was swapped, nothing to do.
            return Ok(());
        }
        return Err(format!(
            "the application delegate ({}) already handles application:openURLs:",
            class.name().to_string_lossy()
        ));
    }
    // SAFETY: the function has the signature AppKit expects for `application:openURLs:` (see
    // `OPEN_URLS_ENCODING`), so erasing its type to `Imp` is sound.
    let imp: Imp =
        unsafe { std::mem::transmute::<OpenUrlsImp, Imp>(application_open_urls as OpenUrlsImp) };
    // SAFETY: `class` is a registered class, the implementation matches the encoding string,
    // and the class was just checked not to respond to the selector, so no existing behaviour
    // is replaced.
    let added = unsafe {
        objc2::ffi::class_addMethod(
            (class as *const AnyClass).cast_mut(),
            selector,
            imp,
            OPEN_URLS_ENCODING.as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err("class_addMethod failed".into());
    }

    // NSApplication may cache which optional methods its delegate implements when the delegate
    // is assigned, so assign it again now that the class has grown a method.
    // SAFETY: `setDelegate:` takes one object-or-nil argument and returns nothing; the delegate
    // stays alive because its owner (the windowing library, or the leak above) retains it.
    unsafe {
        let _: () = msg_send![&*app, setDelegate: Option::<&AnyObject>::None];
        let _: () = msg_send![&*app, setDelegate: &*delegate];
    }
    Ok(())
}

/// Registers (once) and returns the method-less delegate class used when no delegate exists;
/// `hook_application_delegate` adds `application:openURLs:` to it like to any other delegate.
fn fallback_delegate_class() -> Result<&'static AnyClass, String> {
    if let Some(class) = AnyClass::get(FALLBACK_DELEGATE_CLASS) {
        return Ok(class);
    }
    ClassBuilder::new(FALLBACK_DELEGATE_CLASS, NSObject::class())
        .map(ClassBuilder::register)
        .ok_or_else(|| "could not create the application delegate class".into())
}

pub(crate) mod sandbox;

#[cfg(test)]
mod tests;
