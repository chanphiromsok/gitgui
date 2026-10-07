#!/bin/sh
# Builds a release gitgui.app (and a .dmg) on a Mac.
# usage: scripts/bundle-macos.sh [--arch arm64|x86_64|universal]
#   no option   builds for the Mac you run this on
#   --arch      builds for Apple Silicon (arm64), Intel (x86_64), or both joined (universal);
#               needs `rustup target add` for the targets, which this script does for you.
#   --universal is the same as --arch universal
# Writes dist/gitgui.app and dist/gitgui-VERSION-macos-ARCH.dmg
set -eu
cd "$(dirname "$0")/.."

ARCH=native
case "${1:-}" in
    --universal) ARCH=universal ;;
    --arch) ARCH="${2:?--arch wants arm64, x86_64 or universal}" ;;
    "") ;;
    *) echo "usage: $0 [--arch arm64|x86_64|universal]" >&2; exit 2 ;;
esac

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=dist
APP="$OUT/gitgui.app"
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

ARM=aarch64-apple-darwin
X86=x86_64-apple-darwin
BIN() { echo "target/$1/release/gitgui-app"; }

case "$ARCH" in
    native)
        cargo build --release -p gitgui-app
        cp target/release/gitgui-app "$APP/Contents/MacOS/gitgui"
        ARCH=$(uname -m | sed 's/^arm64$/arm64/')
        ;;
    arm64)
        rustup target add "$ARM"
        cargo build --release -p gitgui-app --target "$ARM"
        cp "$(BIN $ARM)" "$APP/Contents/MacOS/gitgui"
        ;;
    x86_64)
        rustup target add "$X86"
        cargo build --release -p gitgui-app --target "$X86"
        cp "$(BIN $X86)" "$APP/Contents/MacOS/gitgui"
        ;;
    universal)
        rustup target add "$ARM" "$X86"
        cargo build --release -p gitgui-app --target "$ARM"
        cargo build --release -p gitgui-app --target "$X86"
        lipo -create -output "$APP/Contents/MacOS/gitgui" "$(BIN $ARM)" "$(BIN $X86)"
        ;;
    *) echo "unknown --arch $ARCH (arm64, x86_64 or universal)" >&2; exit 2 ;;
esac

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

DMG="$OUT/gitgui-$VERSION-macos-$ARCH.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname gitgui -srcfolder "$APP" -ov -format UDZO "$DMG"
echo "Built $APP and $DMG"
