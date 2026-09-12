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
raw="$(mktemp -t diskscope-shot).bmp"
trap 'rm -f "$raw"' EXIT

cargo build --release --manifest-path "$here/Cargo.toml" -p diskscope
"$here/target/release/diskscope" --screenshot "$raw" "$target"

mkdir -p "$(dirname "$out")"
sips -s format png -Z "$width" "$raw" --out "$out" >/dev/null
echo "wrote $out ($(du -h "$out" | cut -f1), scanned $target)"
