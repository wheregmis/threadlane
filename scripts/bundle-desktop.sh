#!/bin/sh
# Builds the desktop app and wraps it as target/Threadlane.app, ad-hoc
# signed. macOS shows notifications only from an app bundle.
#
#   scripts/bundle-desktop.sh            # release build
#   PROFILE=debug scripts/bundle-desktop.sh
set -eu

cd "$(dirname "$0")/.."
profile="${PROFILE:-release}"
cargo="${CARGO:-cargo}"
if [ "$profile" = release ]; then
    "$cargo" build --release -p threadlane-desktop
else
    "$cargo" build -p threadlane-desktop
fi

app=target/Threadlane.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "target/$profile/threadlane-desktop" "$app/Contents/MacOS/threadlane-desktop"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>dev.threadlane.desktop</string>
    <key>CFBundleName</key>
    <string>Threadlane</string>
    <key>CFBundleExecutable</key>
    <string>threadlane-desktop</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$app"
echo "$app"
