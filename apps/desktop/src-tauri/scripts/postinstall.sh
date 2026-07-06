#!/bin/sh
# deb/rpm post-install: register the Firefox native-messaging host system-wide
# and refresh the desktop/MIME databases.
set -e

MANIFEST_DIR=/usr/lib/mozilla/native-messaging-hosts
mkdir -p "$MANIFEST_DIR"
cat > "$MANIFEST_DIR/com.ldm.host.json" <<'JSON'
{
  "name": "com.ldm.host",
  "description": "Linux Download Manager native bridge",
  "path": "/usr/bin/ldm-native-host",
  "type": "stdio",
  "allowed_extensions": ["ldm@ramazan.dev"]
}
JSON
chmod 0644 "$MANIFEST_DIR/com.ldm.host.json"

# Also install for a system-wide Chromium later (harmless if absent).
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
exit 0
