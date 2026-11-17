#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target_dir="${CARGO_TARGET_DIR:-target}"
version="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "threadlane-gpui") | .version')"
architecture="$(uname -m)"
app="$target_dir/release/Threadlane.app"
contents="$app/Contents"
archive="$target_dir/release/Threadlane-${version}-${architecture}.zip"
dmg="$target_dir/release/Threadlane-${version}-${architecture}.dmg"
iconset="$(mktemp -d)/Threadlane.iconset"
trap 'rm -rf -- "$(dirname "$iconset")"' EXIT

cargo build --locked --release --bin threadlane-gpui
rm -rf "$app" "$archive" "$dmg"
mkdir -p "$contents/MacOS" "$contents/Resources" "$iconset"
install -m755 "$target_dir/release/threadlane-gpui" "$contents/MacOS/threadlane"
cp packaging/Info.plist "$contents/Info.plist"
plutil -replace CFBundleExecutable -string threadlane "$contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$version" "$contents/Info.plist"
plutil -replace CFBundleVersion -string "$version" "$contents/Info.plist"

for size in 16 32 128 256 512; do
  sips -z "$size" "$size" resources/icon_512.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" resources/icon_512.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$contents/Resources/Threadlane.icns"

xattr -cr "$app"
identity="${THREADLANE_SIGNING_IDENTITY:--}"
if [[ "$identity" == "-" ]]; then
  codesign --force --deep --sign - "$app"
else
  codesign --force --deep --options runtime --timestamp \
    --entitlements packaging/Entitlements.plist --sign "$identity" "$app"
fi
codesign --verify --deep --strict --verbose=2 "$app"

ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
create-dmg \
  --volname Threadlane \
  --window-size 960 540 \
  --icon-size 128 \
  --icon Threadlane.app 200 250 \
  --app-drop-link 760 250 \
  "$dmg" "$app"

printf 'Created %s\nCreated %s\n' "$archive" "$dmg"
