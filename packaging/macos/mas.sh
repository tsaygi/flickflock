#!/bin/sh
# Builds the Mac App Store package: a sandboxed Universal app in dist/mas.
#
#   packaging/macos/mas.sh --local                     # ad-hoc signed, for testing the sandbox
#   MAS_PROFILE=path/to.provisionprofile BUILD_NUMBER=1 packaging/macos/mas.sh
#
# BUILD_NUMBER must increase with every upload of the same version. The team comes from the
# profile; the app is signed with the certificate the profile names and the package with the
# team's "3rd Party Mac Developer Installer" certificate from the keychain. Override them with
# MAS_APP_IDENTITY and MAS_INSTALLER_IDENTITY.
set -eu
cd "$(dirname "$0")/../.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD_NUMBER=${BUILD_NUMBER:-1}
BUNDLE_ID=org.saygi.flickflock
ENTITLEMENTS=packaging/macos/FlickFlock.entitlements
OUT=dist/mas
APP=$OUT/FlickFlock.app
PKG=$OUT/FlickFlock-$VERSION-$BUILD_NUMBER.pkg

LOCAL=0
[ "${1:-}" = "--local" ] && LOCAL=1
if [ "$LOCAL" = 0 ] && [ ! -f "${MAS_PROFILE:-}" ]; then
    echo "Set MAS_PROFILE to the Mac App Store provisioning profile for $BUNDLE_ID" >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=11.0
# A pinned rustup toolchain goes first in PATH: a Homebrew cargo has no cross-compile targets.
TOOLCHAIN=${MAS_TOOLCHAIN:-1.100.0-aarch64-apple-darwin}
rustup toolchain install "$TOOLCHAIN" --profile minimal --no-self-update \
    --target aarch64-apple-darwin --target x86_64-apple-darwin >/dev/null 2>&1
PATH="$(dirname "$(rustup which --toolchain "$TOOLCHAIN" rustc)"):$PATH"
# Keep local paths out of the shipped binary. Apple's strip runs below because the toolchain's
# rust-objcopy cannot find libLLVM when cargo is run this way.
export RUSTFLAGS="--remap-path-prefix=$HOME=/build"
export CARGO_PROFILE_RELEASE_STRIP=false
cargo build --release --locked -p flickflock --target aarch64-apple-darwin
cargo build --release --locked -p flickflock --target x86_64-apple-darwin

rm -rf "$OUT"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/flickflock" \
    target/aarch64-apple-darwin/release/flickflock \
    target/x86_64-apple-darwin/release/flickflock
strip -S -x "$APP/Contents/MacOS/flickflock"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUILD@/$BUILD_NUMBER/g" \
    packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
plutil -lint -s "$APP/Contents/Info.plist"
# The strings live in the binary; the .lproj folders are how macOS and the App Store see the languages.
for lang in en $(sed -n 's/.*include_str.*locales\/\(.*\)\.toml.*/\1/p' crates/ff-core/src/i18n.rs); do
    mkdir -p "$APP/Contents/Resources/$lang.lproj"
    printf '"CFBundleDisplayName" = "FlickFlock";\n' > "$APP/Contents/Resources/$lang.lproj/InfoPlist.strings"
done

if [ "$LOCAL" = 1 ]; then
    codesign --force --sign - --entitlements "$ENTITLEMENTS" "$APP"
    echo "Built $APP (ad-hoc, not uploadable)"
    exit 0
fi

cp "$MAS_PROFILE" "$APP/Contents/embedded.provisionprofile"
# A downloaded profile carries com.apple.quarantine, which App Store Connect rejects (ITMS-91109).
xattr -cr "$APP"
TEAM_ID=$(security cms -D -i "$MAS_PROFILE" | plutil -extract TeamIdentifier.0 raw -o - -)
# Sign with the certificate the profile names: a team can hold several with the same name.
APP_IDENTITY=${MAS_APP_IDENTITY:-$(security cms -D -i "$MAS_PROFILE" |
    plutil -extract DeveloperCertificates.0 raw -o - - |
    base64 -D | shasum -a 1 | cut -d' ' -f1 | tr a-f A-F)}
INSTALLER_IDENTITY=${MAS_INSTALLER_IDENTITY:-$(security find-identity -v |
    sed -n "s/.*\"\(3rd Party Mac Developer Installer: .* ($TEAM_ID)\)\"$/\1/p" | head -1)}
if [ -z "$INSTALLER_IDENTITY" ]; then
    echo "No \"3rd Party Mac Developer Installer\" certificate for team $TEAM_ID in the keychain" >&2
    exit 1
fi
SIGNED=$(mktemp)
trap 'rm -f "$SIGNED"' EXIT
cp "$ENTITLEMENTS" "$SIGNED"
/usr/libexec/PlistBuddy \
    -c "Add :com.apple.application-identifier string $TEAM_ID.$BUNDLE_ID" \
    -c "Add :com.apple.developer.team-identifier string $TEAM_ID" "$SIGNED"
codesign --force --timestamp --options runtime --sign "$APP_IDENTITY" --entitlements "$SIGNED" "$APP"
codesign --verify --strict --deep "$APP"
productbuild --component "$APP" /Applications --sign "$INSTALLER_IDENTITY" "$PKG"
echo "Built $PKG"
