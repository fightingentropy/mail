#!/bin/bash
# Builds Mail.app for Apple Silicon and, with --install, puts it in
# /Applications. --open launches it afterwards.
#
#   packaging/macos/build-app.sh [--install] [--open]
#
# The app is ad-hoc signed for this Mac and not notarized. Set
# CODESIGN_IDENTITY to sign with a Developer ID instead.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
install=false
open_after=false
for argument in "$@"; do
    case "$argument" in
        --install) install=true ;;
        --open) open_after=true ;;
        *) echo "usage: $0 [--install] [--open]" >&2; exit 2 ;;
    esac
done

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
codesign --force --sign "${CODESIGN_IDENTITY:--}" "$app"
codesign --verify --strict "$app"
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
if $open_after; then
    open "$target"
fi
