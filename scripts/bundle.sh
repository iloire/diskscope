#!/usr/bin/env bash
# Wraps the release binary in Diskscope.app.
#
# The bundle is not cosmetic: macOS grants Full Disk Access to an application,
# and without it a scan of / silently skips most of ~/Library, /System/Data and
# every other protected path. A bare binary in target/release can be granted
# access too, but the grant is lost the moment you rebuild, whereas the bundle
# keeps it.
set -euo pipefail

# A .app is a macOS construct and the Full Disk Access grant it exists for has
# no counterpart elsewhere; on Linux the release binary is the deliverable.
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "bundle.sh only applies to macOS; use target/release/diskscope directly" >&2
  exit 1
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app="${1:-$here/Diskscope.app}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/Cargo.toml" | head -1)"
commit="$(git -C "$here" rev-parse --short=7 HEAD 2>/dev/null || echo unknown)"

cargo build --release --manifest-path "$here/Cargo.toml" -p diskscope

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$here/target/release/diskscope" "$app/Contents/MacOS/diskscope"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>Diskscope</string>
  <key>CFBundleDisplayName</key>       <string>Diskscope</string>
  <key>CFBundleIdentifier</key>        <string>com.iloire.diskscope</string>
  <key>CFBundleExecutable</key>        <string>diskscope</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key>           <string>$commit</string>
  <key>LSMinimumSystemVersion</key>    <string>11.0</string>
  <key>NSHighResolutionCapable</key>   <true/>
  <key>LSApplicationCategoryType</key> <string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

# An unsigned bundle copied around gets quarantined; ad-hoc signing is enough
# to launch it locally and keeps the Full Disk Access grant stable.
codesign --force --sign - "$app" >/dev/null 2>&1 || \
  echo "note: could not ad-hoc sign; the bundle still runs from here" >&2

echo "built $app ($version, $commit)"
echo
echo "To scan the whole disk, grant it Full Disk Access:"
echo "  System Settings > Privacy & Security > Full Disk Access > + > $app"
