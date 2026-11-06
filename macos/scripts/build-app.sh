#!/bin/bash
# Build "RDP Launcher.app" and a drag-to-Applications disk image.
# Run from the macos/ folder:  ./scripts/build-app.sh
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
target="aarch64-apple-darwin"
app="dist/RDP Launcher.app"
dmg="dist/RDP-Launcher-$version.dmg"

echo "==> Building release binary ($target)"
rustup target add "$target" >/dev/null 2>&1 || true
cargo build --release --target "$target"

echo "==> Assembling $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$target/release/rdp-launcher" "$app/Contents/MacOS/rdp-launcher"
cp resources/Info.plist "$app/Contents/Info.plist"
cp resources/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"

echo "==> Signing (ad-hoc)"
codesign --force --deep --sign - "$app"
codesign --verify --verbose "$app"

echo "==> Creating $dmg"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -volname "RDP Launcher" -srcfolder "$stage" -ov -format UDZO "$dmg" >/dev/null

echo
echo "App: $app ($(du -sh "$app" | cut -f1))"
echo "DMG: $dmg ($(du -sh "$dmg" | cut -f1))"
