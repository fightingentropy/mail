#!/bin/bash
# Builds Mail.app for Apple Silicon.
#
#   packaging/macos/build-app.sh [--install] [--link-cli] [--open] [--notarize PROFILE]
#
#   --install            put the app in /Applications
#   --link-cli           make ~/.cargo/bin/mail the app's own binary, so the
#                        terminal and the window share one signed identity
#   --open               launch it afterwards
#   --notarize PROFILE   notarize and staple with a `xcrun notarytool
#                        store-credentials` profile (needs a Developer ID)
#
# Signing uses CODESIGN_IDENTITY, else the first "Developer ID Application"
# identity in the keychain, else an ad-hoc signature. A stable identity lets
# macOS remember "Always Allow" for Mail's Keychain item across rebuilds; an
# ad-hoc signature changes with every build, so macOS asks again.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
install=false
link_cli=false
open_after=false
notary_profile=""
while [ $# -gt 0 ]; do
    case "$1" in
        --install) install=true ;;
        --link-cli) link_cli=true ;;
        --open) open_after=true ;;
        --notarize) notary_profile="${2:?--notarize needs a notarytool profile name}"; shift ;;
        *) echo "usage: $0 [--install] [--link-cli] [--open] [--notarize PROFILE]" >&2; exit 2 ;;
    esac
    shift
done

identity="${CODESIGN_IDENTITY:-$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)}"
if [ -n "$notary_profile" ] && [ -z "$identity" ]; then
    echo "Notarizing needs a Developer ID Application identity." >&2
    exit 1
fi

cd "$root"
cargo build --release --locked -p mail
binary="target/aarch64-apple-darwin/release/mail"
test "$(lipo -archs "$binary")" = arm64
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

app="dist/Mail.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/mail"
sed "s/__VERSION__/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
cp LICENSE "$app/Contents/Resources/LICENSE"

# iconutil wants each size and its @2x version; the icon is drawn by
# `cargo run -p mail --example render_icon`.
iconset="$(mktemp -d)/Mail.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$here/icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$here/icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Mail.icns"

# codesign rejects Finder metadata inherited from the checkout.
xattr -cr "$app"
if [ -n "$identity" ]; then
    # Hardened runtime and a secure timestamp: what notarization requires.
    codesign --force --timestamp --options runtime --sign "$identity" "$app"
    echo "Signed with $identity"
else
    codesign --force --sign - "$app"
    echo "Signed ad hoc (no Developer ID found)"
fi
codesign --verify --strict "$app"

if [ -n "$notary_profile" ]; then
    archive="$(mktemp -d)/Mail.zip"
    ditto -c -k --keepParent "$app" "$archive"
    xcrun notarytool submit "$archive" --keychain-profile "$notary_profile" --wait
    xcrun stapler staple "$app"
    spctl --assess --type execute --verbose "$app"
fi
echo "Built $app ($version)"

target="$app"
if $install; then
    target="/Applications/Mail.app"
    # Apple's Mail lives in /System/Applications; never replace any other
    # app that happens to sit here under the same name.
    if [ -d "$target" ]; then
        existing="$(defaults read "$target/Contents/Info" CFBundleIdentifier 2>/dev/null || true)"
        if [ "$existing" != "org.erlin.mail" ]; then
            echo "$target belongs to ${existing:-another app}; not replacing it." >&2
            exit 1
        fi
        if pgrep -qf "$target/Contents/MacOS/mail"; then
            osascript -e 'quit app id "org.erlin.mail"' >/dev/null 2>&1 || true
            sleep 1
        fi
        rm -rf "$target"
    fi
    ditto "$app" "$target"
    echo "Installed $target"
fi
if $link_cli; then
    mkdir -p "$HOME/.cargo/bin"
    ln -sf "$(cd "$(dirname "$target")" && pwd)/Mail.app/Contents/MacOS/mail" "$HOME/.cargo/bin/mail"
    echo "Linked ~/.cargo/bin/mail to the app"
fi
if $open_after; then
    open "$target"
fi
