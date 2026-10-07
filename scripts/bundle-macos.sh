#!/bin/sh
# Builds a release gitgui.app (and a .dmg) for the Mac you run this on.
# usage: scripts/bundle-macos.sh [--universal]
#   --universal  builds for Apple Silicon and Intel and joins them (needs both rustup targets)
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=dist
APP="$OUT/gitgui.app"
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

if [ "${1:-}" = "--universal" ]; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cargo build --release -p gitgui-app --target aarch64-apple-darwin
    cargo build --release -p gitgui-app --target x86_64-apple-darwin
    lipo -create -output "$APP/Contents/MacOS/gitgui" \
        target/aarch64-apple-darwin/release/gitgui-app target/x86_64-apple-darwin/release/gitgui-app
else
    cargo build --release -p gitgui-app
    cp target/release/gitgui-app "$APP/Contents/MacOS/gitgui"
fi

# The icon: crates/app/assets/app-icon/icon.svg, drawn to AppIcon.icns by the app_icon example.
cp crates/app/assets/app-icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>gitgui</string>
  <key>CFBundleDisplayName</key><string>gitgui</string>
  <key>CFBundleIdentifier</key><string>dev.gitgui.app</string>
  <key>CFBundleExecutable</key><string>gitgui</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

# Ad-hoc signature: enough to run on this Mac. For other Macs see "Signing" in the README.
codesign --force --deep --sign - "$APP"

rm -f "$OUT/gitgui-$VERSION.dmg"
hdiutil create -quiet -volname gitgui -srcfolder "$APP" -ov -format UDZO "$OUT/gitgui-$VERSION.dmg"
echo "Built $APP and $OUT/gitgui-$VERSION.dmg"
