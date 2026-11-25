# Decisions

Non-obvious choices, interpretations of ambiguous requirements, and known limitations.

## Architecture

- **Three crates.** `ff-core` has no GUI or OS dependency and holds everything that can be unit-tested headlessly. `ff-platform` holds the OS bindings behind `PlatformDecoder` / `PlatformIntegration`. `ff-app` is the only crate that knows about egui/wgpu.
- **The app builds the winit event loop itself** (`eframe::create_native`) instead of calling `eframe::run_native`, because the macOS open-file handler has to be installed after the event loop exists and before it runs. On macOS, Finder delivers opened files as an Apple Event, not as arguments; the handler adds `application:openURLs:` to the application delegate at runtime.
- **Wanted lists instead of job queues.** The loader and the thumbnail service are told "this is what I need now, most important first" every time that changes. Queued work that is no longer wanted disappears and running work gets its cancel flag set. This is what keeps a held-down arrow key or a fast strip scroll from building a backlog.
- **Two representations per image.** A *preview* (longest side = the largest monitor dimension) is what gets cached and prefetched; the *full-resolution* image is decoded only when the view is zoomed past what the preview resolves, and is kept only for the image on screen. The cache budget therefore holds many images instead of a few 50-megapixel ones.
- **Folder scans, neighbour-folder searches, metadata, edits, saves and clipboard copies run on worker threads** and report back through a channel. The UI thread uploads finished pixels to the GPU and draws.

## Rendering

- **Textures hold premultiplied, gamma-encoded RGBA8** (`Rgba8Unorm`). Mipmaps and blending are computed in gamma space, like most viewers, rather than in linear light.
- **Tiles do not overlap.** Images larger than the GPU texture limit are split into tiles that meet exactly, each with its own mip chain and clamp-to-edge sampling. Filtering does not interpolate across a tile boundary; the half-texel difference is not visible in practice and avoids gutter bookkeeping.
- **Mipmaps are built on the CPU** in the decode worker (box filter), not on the GPU.
- **Transitions** render the outgoing and incoming images into two off-screen textures and blend them with one fragment shader per transition (`crates/ff-app/src/shaders/transitions`). Adding a transition means adding one `.wgsl` file and one line in `canvas.rs` and `ff_core::config::TRANSITIONS`.
- **Placeholders are drawn by egui.** While an image decodes (and while skimming) its thumbnail is drawn stretched through egui's painter; the real image always goes through the wgpu canvas.
- **HUD translucency is a semi-transparent panel on every platform.** OS vibrancy (macOS) / Mica (Windows) blurs what is behind the *window*, not the image behind the HUD, so it is not used. A blur of the image under the HUD is a possible later addition.

## Behaviour

- **Double-click** resets the view when it is zoomed or panned, and toggles full screen when it is not. (The brief asked for both meanings.)
- **Scrolling.** A wheel follows the setting (zoom, or change image). Precise trackpad scrolling pans when the image is larger than the window and otherwise follows the setting. `Mod`+scroll always zooms; over the thumbnail strip it resizes thumbnails.
- **Space over the thumbnail strip means Peek**, not Next. Peek shows the hovered thumbnail's image while Space is held; releasing returns unless you click.
- **Skim** maps the pointer's horizontal position over the image area to a position in the folder while the Skim key is held. Thumbnails are shown while the pointer moves; the full preview loads once it rests for 120 ms. Releasing stays on that image and records one history entry.
- **History** records jumps, not every step: flicking sequentially updates the current entry, so Back returns to where you jumped from rather than one image back.
- **Key repeat is done by the app**, at the configured rate, and OS key-repeat events are ignored, so the rate is the same on every platform.
- **Horizontal swipes** fire once per gesture (60 points of travel, then a 350 ms cool-down).
- **Reserved shortcuts** (quit, close window, hide, app/window switcher, …) cannot be bound.
- **Hidden things are skipped:** dot-files and dot-folders, folders with the Windows hidden attribute, and macOS packages such as `.photoslibrary` or `.app`.
- **Tree traversal** is depth-first pre-order inside a root that defaults to the parent of the first opened folder.
- **Date grouping and "date taken" sorting** use the EXIF date when present and the modification time (UTC) otherwise.
- **Unsaved edits block navigation** with a Save / Discard / Keep editing dialog.
- **Editing is applied to the full-resolution image**, decoded when the first edit is made.
- **Saving.** JPEG → JPEG saves whose edits are only rotations/flips rewrite just the EXIF orientation tag (the compressed data is copied byte for byte). Everything else is re-encoded; EXIF is carried over for JPEG, PNG and WebP with orientation reset to 1. Pixels are saved as sRGB, so the source ICC profile is not copied.
- **Move to Trash on macOS uses `NSFileManager`**, not Finder scripting, so no Automation permission prompt appears. The trade-off is that Finder's "Put Back" is not available for those items.
- **Single instance** uses a local socket in the user's cache directory (a named pipe on Windows). With "Open files in the existing window" off, or with `--new-window`, a separate process and window is started.

## macOS privacy prompts

- Desktop, Documents, Downloads and external or network volumes are guarded by a per-folder consent prompt, and any file-system call into them (even `stat` or creating a folder watcher) blocks until the user answers. Nothing that touches a folder therefore runs on the UI thread: checking the opened path, scanning, and creating the watcher all happen on background threads, and a failed scan is retried for a while so browsing starts as soon as access is granted.
- Background jobs run on their own threads, not on the shared CPU pool, so a job that is waiting for a prompt cannot stall decoding.
- Neighbouring folders are not indexed ahead of time when the current folder sits directly under the home folder or a volume root: its siblings are unrelated locations, each with its own prompt. They are looked up when the user actually moves to the next folder.
- The bundle's Info.plist carries usage descriptions for those locations.

## Localisation

- **English text is the key.** UI code calls `t("Open…")` / `tf("Moved {0} to the Trash", &[&name])`; each language is a flat TOML table from English to the translation, embedded in the binary. A missing translation falls back to English instead of showing a key name.
- **Numbered placeholders** (`{0}`, `{1}`) let translations reorder values.
- **Coverage is enforced by tests.** One test scans the app's source for `t`/`tf` calls (plus labels that come from `ff-core`) and fails when any locale lacks a string or carries an unused one; another checks that every translation keeps its key's placeholders.
- **Sixteen languages, chosen by number of speakers among those the UI toolkit renders correctly.** egui shapes complex scripts (Devanagari, Bengali) but has no bidirectional layout yet, so Arabic, Urdu and Hebrew would show words in the wrong order and are left out until that changes.
- **Glyphs for non-Latin scripts come from the system's fonts**; only Latin text and icons are bundled.
- Not translated: key names in shortcuts, EXIF/XMP tag names, error details produced by decoding libraries and the operating system, and log output.
- The language follows the OS (`sys-locale`) unless set in Settings; it switches immediately, without a restart.

## File associations

- **Settings ▸ File types** lists every format group and lets the user make FlickFlock the default app per type or for all of them. Becoming the default is always the user's explicit choice; installing only registers FlickFlock as a viewer.
- **macOS** uses Launch Services (`LSSetDefaultRoleHandlerForContentType`, via plain C bindings). The OS applies a change a few seconds after the call returns, so the tab shows the new state at once and re-checks eight seconds later. It only works from an app bundle (a bundle identifier is required).
- **Linux** uses `xdg-mime` with the installed `flickflock.desktop` entry.
- **Windows** protects the per-user default-app choice, so applications cannot change it; the tab explains this and opens *Settings ▸ Apps ▸ Default apps*.
- **Undo.** Before taking a type over, the app that opened it is remembered in the config (`[associations.previous]`), and the row offers *Restore previous app*. Types FlickFlock already had when first asked have nothing to restore to.
- **One request per OS type.** Extensions that share a type identifier (`.jpg`/`.jpeg`, `.tif`/`.tiff`) are set once, because macOS may ask the user to confirm each change.

## Decoding

- **Format detection reads magic bytes**; the extension only disambiguates containers (TIFF-based RAW) and formats without a signature (TGA). Folder *indexing* goes by extension so a 2,000-file folder on a network share is not opened file by file.
- **RAW files** are scanned for embedded JPEG streams and the largest one is shown, with the container's orientation.
- **Embedded EXIF thumbnails** are used for the strip when they are at least 160 px; otherwise the image is decoded and scaled to 256 px.
- **Animations** keep composited frames in memory. Above 768 MB of raw frames, all frames are stored QOI-compressed and decompressed on access, which keeps long animations frame-accurate with a bounded footprint. Animation frames are only decoded for the image on screen.
- **Float images (EXR, HDR)** are clamped and encoded with the sRGB curve; there is no tone mapping.
- **ICC profiles** are converted to sRGB with `qcms` for RGB images. Grey and CMYK profiles are not converted.
- **AVIF** is behind the `avif` cargo feature because dav1d needs a C toolchain and library at build time; macOS and Windows fall back to the system codec without it.

## Known limitations

- **Mac App Store builds** ask once for folder access and keep it as a security-scoped bookmark, because the sandbox grants only the opened file. Declining still shows that file. Sibling-folder navigation needs access to the parent folder. The patched winit shows a plain arrow for diagonal-resize, help, zoom and wait cursors, and Ctrl-Tab / Ctrl-Esc no longer reach the app.
- Windows (WIC) and Linux (libheif) HEIC decoding, the Windows/Linux installers and Wayland/X11 behaviour are type-checked and linted from macOS but have not been run on those systems yet; CI is the first place they run.
- On Windows, HEIC orientation relies on the WIC codec applying the container's rotation, and wide-gamut HEICs are not converted to sRGB.
- "Open with" opens the system default application; choosing another application is not implemented.
- Only navigation shortcuts are editable in the Settings window; the others are editable in the config file.
- Wide-gamut and HDR display output, and a pixel-perfect native look per OS, are out of scope for now.

## Dependencies beyond the original list

- `rfd` – native open/save dialogs.
- `sys-locale` – reads the OS language for the default interface language.
- `qoi` – thumbnail cache files and compressed animation frames.
- `parking_lot`, `crossbeam-channel`, `thiserror`, `bytemuck` – small, common building blocks.
