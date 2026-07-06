#!/bin/sh
# deb/rpm post-remove: drop the native-messaging host manifest.
set -e
rm -f /usr/lib/mozilla/native-messaging-hosts/com.ldm.host.json
exit 0
