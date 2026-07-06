#!/usr/bin/env bash
# Assemble per-browser extension packages from the shared source in extension/.
# The two browsers need different manifests (Firefox: event page + blocking
# webRequest + allowed_extensions; Chromium: service worker + allowed_origins),
# but share all JS/HTML/icons.
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="dist-ext"
SHARED="background.js content.js popup.html popup.js options.html options.js icons"

rm -rf "$OUT"
mkdir -p "$OUT/firefox" "$OUT/chrome"

for f in $SHARED; do
  cp -r "extension/$f" "$OUT/firefox/"
  cp -r "extension/$f" "$OUT/chrome/"
done
cp extension/manifest.json         "$OUT/firefox/manifest.json"
cp extension/manifest.chrome.json  "$OUT/chrome/manifest.json"

echo "Firefox → $OUT/firefox"
echo "  Load: about:debugging → This Firefox → Load Temporary Add-on → pick manifest.json"
echo "Chrome/Chromium/Brave/Edge → $OUT/chrome"
echo "  Load: chrome://extensions → enable Developer mode → Load unpacked → pick the folder"
