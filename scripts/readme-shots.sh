#!/usr/bin/env bash
# Takes the README's pictures again from the demo repository (scripts/demo-repo.sh). A picture is as sharp as the
# screen the window opens on: on a Retina screen the capture is 2x, on a 1080p one it is 1x and looks soft in the README.
#
#   scripts/readme-shots.sh            # writes docs/img/*.webp; refuses a 1x capture
#   scripts/readme-shots.sh OUT_DIR    # writes there instead, whatever the screen
#
# Needs ImageMagick (magick) and cwebp: brew install imagemagick webp
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/docs/img}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cargo build -q --release -p gitgui-app --manifest-path "$ROOT/Cargo.toml"
"$ROOT/scripts/demo-repo.sh" "$WORK/acme-app" >/dev/null
mkdir -p "$WORK/shots" "$OUT"

# Each script drives the window and saves pictures of it; the app quits at the end.
for name in hero styles; do
  sed "s|@DEMO@|$WORK/acme-app|" "$ROOT/scripts/readme-shots/$name.steps" > "$WORK/$name.steps"
  GITGUI_DATA_DIR="$WORK/data-$name" GITGUI_SHOTS="$WORK/shots" GITGUI_SCRIPT="$WORK/$name.steps" \
    "$ROOT/target/release/gitgui-app" >/dev/null 2>&1
done

width="$(magick identify -format %w "$WORK/shots/hero.png")"
if [ "$width" -lt 2000 ] && [ $# -eq 0 ]; then
  echo "The capture is ${width}px wide (a 1x screen), so the pictures would look soft." >&2
  echo "Open the window on a Retina screen and run again, or pass an OUT_DIR to write somewhere else." >&2
  exit 1
fi

# Whole-window pictures, no wider than they need to be.
for name in hero review light wizard; do
  magick "$WORK/shots/$name.png" -resize '1800x>' "$WORK/$name.png"
  cwebp -quiet -q 82 "$WORK/$name.png" -o "$OUT/$name.webp"
done

# Six graph styles on one sheet: the top of each window, in a 2x3 grid.
for style in neon soft circuit graphite tokyo gruvbox; do
  magick "$WORK/shots/s_$style.png" -gravity north -chop 0x5% -crop 100%x64%+0+0 +repage "$WORK/tile_$style.png"
done
magick montage "$WORK"/tile_{neon,soft,circuit,graphite,tokyo,gruvbox}.png -tile 2x3 -geometry +12+12 -background '#111111' "$WORK/sheet.png"
magick "$WORK/sheet.png" -resize '2000x>' "$WORK/styles.png"
cwebp -quiet -q 80 "$WORK/styles.png" -o "$OUT/styles.webp"

echo "Wrote hero, review, light, wizard and styles to $OUT"
