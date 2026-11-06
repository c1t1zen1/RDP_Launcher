#!/bin/bash
# Build resources/AppIcon.icns from the Windows icon (../assets/app.ico, 256 px).
# Run from the macos/ folder:  ./scripts/make-icon.sh
set -euo pipefail
cd "$(dirname "$0")/.."

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
iconset="$work/AppIcon.iconset"
mkdir -p "$iconset"

sips -s format png ../assets/app.ico --out "$work/source.png" >/dev/null
for spec in 16:icon_16x16 32:icon_16x16@2x 32:icon_32x32 64:icon_32x32@2x \
            128:icon_128x128 256:icon_128x128@2x 256:icon_256x256; do
    size="${spec%%:*}"
    name="${spec#*:}"
    sips -z "$size" "$size" "$work/source.png" --out "$iconset/$name.png" >/dev/null
done
iconutil -c icns "$iconset" -o resources/AppIcon.icns
echo "wrote resources/AppIcon.icns"
