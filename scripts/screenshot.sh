#!/usr/bin/env bash
# Regenerates docs/screenshot.png.
#
# The app photographs its own framebuffer (--screenshot, via egui's
# ViewportCommand::Screenshot) rather than going through `screencapture`, which
# needs Screen Recording permission and so cannot be scripted on a fresh
# machine. It scans, lets the map render, selects its own largest find so the
# readout band has something in it, saves, and quits.
#
# Note the image will contain real paths from whatever you point it at.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="${1:-$HOME}"
out="${2:-$here/docs/screenshot.png}"
width="${WIDTH:-1600}"
# A directory rather than `mktemp -t`: BSD and GNU mktemp disagree about both
# `-t` and a suffix after the X's, and a fixed name inside a temp dir sidesteps
# the whole argument.
tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/diskscope-shot.XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT
raw="$tmpdir/shot.bmp"

cargo build --release --manifest-path "$here/Cargo.toml" -p diskscope
"$here/target/release/diskscope" --screenshot "$raw" "$target"

mkdir -p "$(dirname "$out")"
# `sips` ships with macOS; ImageMagick is the equivalent everywhere else. Both
# forms scale the long edge to $width and only ever shrink.
if command -v sips >/dev/null 2>&1; then
  sips -s format png -Z "$width" "$raw" --out "$out" >/dev/null
elif command -v magick >/dev/null 2>&1; then
  magick "$raw" -resize "${width}x${width}>" "$out"
elif command -v convert >/dev/null 2>&1; then
  convert "$raw" -resize "${width}x${width}>" "$out"
else
  echo "need sips (macOS) or ImageMagick to write $out" >&2
  exit 1
fi
echo "wrote $out ($(du -h "$out" | cut -f1), scanned $target)"
