#!/bin/sh
# Builds dist/FlickFlock.app and dist/FlickFlock-<version>.dmg.
#
#   packaging/macos/bundle.sh            # Universal (arm64 + x86_64)
#   packaging/macos/bundle.sh --native   # host architecture only (faster)
#
# The bundle is ad-hoc signed, which is enough for personal use (right-click
# > Open the first time). For distribution, sign with a Developer ID and
# notarize instead.
set -eu
cd "$(dirname "$0")/../.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/FlickFlock.app
FEATURES=${FLICKFLOCK_FEATURES:-}

build() {
    if [ -n "$FEATURES" ]; then
        cargo build --release -p flickflock --features "$FEATURES" "$@"
    else
        cargo build --release -p flickflock "$@"
    fi
}

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

if [ "${1:-}" = "--native" ]; then
    build
    cp target/release/flickflock "$APP/Contents/MacOS/flickflock"
else
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    build --target aarch64-apple-darwin
    build --target x86_64-apple-darwin
    lipo -create -output "$APP/Contents/MacOS/flickflock" \
        target/aarch64-apple-darwin/release/flickflock \
        target/x86_64-apple-darwin/release/flickflock
fi

sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUILD@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
codesign --force --sign - "$APP"

DMG="dist/FlickFlock-$VERSION.dmg"
rm -f "$DMG"
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname FlickFlock -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
echo "Built $APP and $DMG"
