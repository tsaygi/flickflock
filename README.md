<p align="center"><img src="assets/icon-256.png" width="128" alt="FlickFlock icon"></p>

# FlickFlock

**Flick through a flock of photos.** FlickFlock is a fast, keyboard-driven image viewer for macOS, Windows and Linux, written in Rust.

Open a photo, then flick through every photo in that folder — and on into the neighbouring folders — with left/right keys you can remap to anything you like.

## Features

- **Instant folder browsing** – opening one file indexes its whole folder in the background; the next images are prefetched, so flipping has no visible delay.
- **Folder-to-folder navigation** – at the end of a folder, Next continues into the next sibling folder (or the whole directory tree, depth-first). Empty and hidden folders are skipped.
- **Your keys** – every navigation action can have several bindings: any key, mouse buttons 4/5 or horizontal swipes, bound by physical key position or by character, with conflict detection and import/export.
- **GPU canvas** – zoom with pinch, scroll or keys; pan freely past the window edges; mipmapped down-sampling, crisp pixels when zoomed in, and tiling for images larger than the GPU texture limit.
- **Auto-hiding HUD** – nothing but the image when the mouse is still.
- **Thumbnail strip** – virtualised, generated in parallel, cached on disk, with sections for grouped folders.
- **Skim and Peek** – hold `S` and sweep the mouse to scrub through the folder; hold `Space` over a thumbnail to peek at it.
- **Animations** – GIF, animated WebP and APNG with play/pause, a frame-accurate timeline, frame stepping, reverse play and 0.25×–4× speed.
- **Metadata** – EXIF, GPS, XMP and file facts in a side panel; pin any property onto the image.
- **Sorting and grouping** – natural name order (`img2` before `img10`), dates, size, dimensions, type; nested grouping.
- **History** – browser-style Back / Forward.
- **Slideshow** – interval, shuffle, loop and ten GPU transitions (crossfade, slide, push, zoom, wipe, iris, page curl, ripple, pixelate, dissolve).
- **Quick edits** – rotate, flip, crop with aspect presets, extend canvas; undo/redo; Save / Save As with EXIF kept; lossless JPEG rotation.
- **File operations** – move to Trash, rename, reveal in the file manager, copy image or path; the folder is watched and updates live.

### Languages

The interface follows your system language and can be switched under **Settings ▸ General ▸ Language**:

English, 简体中文, हिन्दी, Español, Français, বাংলা, Português, Русский, Bahasa Indonesia, Deutsch, 日本語, Türkçe, 한국어, Tiếng Việt, Italiano, Polski.

Translations live in `crates/ff-core/locales/<code>.toml` as plain `"English" = "translation"` tables; corrections are very welcome. A test fails if any language is missing a string the app can show.

### Formats

JPEG, PNG/APNG, GIF, WebP, TIFF, BMP, ICO, TGA, PNM, QOI, OpenEXR, Radiance HDR, DDS, Farbfeld, JPEG XL and PSD (flattened) are decoded in pure Rust. HEIC/HEIF uses the system codec (ImageIO on macOS, WIC on Windows, `libheif` on Linux when installed). AVIF opens through the system codec on macOS and Windows, or everywhere when built with `--features avif`. Camera RAW files (CR2, CR3, NEF, ARW, RAF, ORF, DNG, RW2, …) show their embedded preview. Formats are detected by content, not by file extension.

## Install

Pre-built packages are attached to CI runs and releases. To build from source you need a stable [Rust toolchain](https://rustup.rs), version 1.95 or newer (`rustup update stable`):

```sh
cargo run --release -p flickflock -- path/to/photo.jpg
```

On Linux install the usual windowing headers first, e.g. on Debian/Ubuntu:

```sh
sudo apt install libgtk-3-dev libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

See [packaging/README.md](packaging/README.md) for installers, file associations and signing.

## Usage

```
flickflock [OPTIONS] [FILE or FOLDER]...

  -n, --new-window  Open a new window even if FlickFlock is already running
```

Drop a file or folder on the window, or use **Open…**. By default a second launch hands its file to the window that is already open.

### Default shortcuts

`Mod` is ⌘ on macOS and Ctrl on Windows/Linux.

| Action | Shortcut |
|---|---|
| Next / previous image | → / ←, Space / Shift+Space |
| Next / previous folder | Alt+→ / Alt+← |
| First / last image | Home / End |
| Fit / actual size | Mod+0 / Mod+1 |
| Zoom in / out | Mod+= / Mod+−, pinch, scroll |
| Full screen | F (also F11 on Windows/Linux), double-click |
| Thumbnails / info panel | T / I |
| Back / forward | Mod+[ / Mod+], mouse buttons 4 / 5 |
| Skim | hold S and move the mouse |
| Peek | hold Space over a thumbnail |
| Animation: play/pause, step, reverse | P, `,` / `.`, R |
| Rotate left / right, flip | Mod+L / Mod+R, H / V |
| Crop | C (Enter applies, Esc cancels) |
| Undo / redo, save, save as | Mod+Z / Mod+Shift+Z, Mod+S, Mod+Alt+S |
| Move to Trash | Mod+Backspace (macOS), Delete (Windows/Linux) |
| Rename / reveal | F2 / Mod+Shift+R |
| Copy image / path | Mod+C / Mod+Shift+C |
| Slideshow | Mod+Shift+S |
| Settings | Mod+, |

Navigation keys are edited in **Settings ▸ Keys**. Every other shortcut can be changed in the config file.

### Configuration

Settings live in a TOML file that the Settings window edits; manual edits are read at startup and anything invalid falls back to its default.

| OS | Location |
|---|---|
| macOS | `~/Library/Application Support/flickflock/config.toml` |
| Windows | `%APPDATA%\flickflock\config.toml` |
| Linux | `~/.config/flickflock/config.toml` |

`FLICKFLOCK_CONFIG=/some/file.toml` overrides the location. Thumbnails are cached in the OS cache directory (clear them under **Settings ▸ Performance**).

## Project layout

```
crates/
  ff-core/      platform-independent logic, no GUI: folder index, navigation, key bindings,
                decoding, caches, animation, metadata, editing, config
  ff-platform/  thin per-OS layer behind traits: system image codecs, macOS open-file events
  ff-app/       the `flickflock` binary: eframe/egui UI, wgpu canvas and WGSL shaders
assets/         generated icons
packaging/      bundle and installer configuration
tests/fixtures/ fixture generator (nothing binary is committed)
```

Design decisions and known limitations are recorded in [DECISIONS.md](DECISIONS.md).

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench -p ff-core                 # thumbnail and next-image latency
tests/fixtures/generate.sh             # sample folders in tests/fixtures/out
```

Set `FLICKFLOCK_LOG=flickflock=debug,ff_core=debug` for timing logs. For scripted smoke tests, `FLICKFLOCK_SELFTEST` runs a list of steps (actions, waits, screenshots); see `crates/ff-app/src/selftest.rs`:

```sh
FLICKFLOCK_SELFTEST="ready;next_image;ready;state;shot:/tmp/shot.png;quit" \
  cargo run -p flickflock -- --new-window tests/fixtures/out/Trip/Day1/photo1.jpg
```

## Roadmap

Full RAW development, wide-gamut/HDR output, compare mode, histogram and colour picker, batch rename/convert, ratings in XMP sidecars, rebinding every shortcut from the UI, right-to-left languages.

## License

[MIT](LICENSE)
