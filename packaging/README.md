# Packaging

| Platform | Output | How |
|---|---|---|
| macOS | `dist/FlickFlock.app`, `dist/FlickFlock-<version>.dmg` (Universal: arm64 + x86_64) | `packaging/macos/bundle.sh` (`--native` for the host architecture only) |
| Windows | `.msi` and setup `.exe` | `cargo install cargo-packager --locked` then `cargo packager --release -p flickflock --formats wix,nsis` |
| Linux | `.deb`, `AppImage` | `cargo packager --release -p flickflock --formats deb,appimage` |
| Linux | `.rpm` | `cargo install cargo-generate-rpm` then `cargo build --release -p flickflock && cargo generate-rpm -p crates/ff-app` |

The cargo-packager and cargo-generate-rpm settings live in `crates/ff-app/Cargo.toml`.

## File associations

FlickFlock registers as a *viewer* (never as the forced default) for every format it can open. The easiest way to make it the default is **Settings ▸ File types** inside the app (per type, or all at once; on Windows it opens the system's Default apps page). The manual routes:

- **macOS** – `CFBundleDocumentTypes` in `packaging/macos/Info.plist`. Pick FlickFlock under *Get Info ▸ Open with ▸ Change All…*.
- **Windows** – the installers write the ProgID / "Open with" entries from `file-associations`. Choose FlickFlock under *Settings ▸ Apps ▸ Default apps*.
- **Linux** – `MimeType=` in `packaging/linux/flickflock.desktop`. For example: `xdg-mime default flickflock.desktop image/jpeg`.

## Signing

Builds are unsigned (ad-hoc signed on macOS), which is fine for personal use:

- macOS: right-click the app and choose *Open* the first time.
- Windows: *More info ▸ Run anyway* in the SmartScreen dialog.

For wider distribution use Developer ID signing + notarization on macOS and Authenticode on Windows.

## Icons

`assets/icon*.{png,ico,icns}` are generated, not drawn by hand:

    cargo run --release -p ff-core --example make_icons -- assets

## Optional features

- `--features avif` enables AVIF decoding through dav1d on every platform (needs `libdav1d` at build time). Without it, AVIF still opens on macOS and Windows through the system codecs.
- HEIC/HEIF uses the OS codec: ImageIO on macOS, WIC on Windows (install the free *HEIF Image Extensions*), and `libheif` on Linux when it is installed.
