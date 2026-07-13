# Packaging

- `com.minidownloader.host.json` — Firefox native-messaging host manifest. For `.deb`/`.rpm`
  the post-install script writes it (with the real host path) to
  `/usr/lib/mozilla/native-messaging-hosts/`. For AppImage the app registers a
  per-user copy on first run (see `register_native_host_manifests`).
- `aria2/` — aria2 is GPLv2; distributing the `aria2c` binary requires shipping
  the corresponding source or a written offer. Place the pinned aria2 source
  tarball (or `WRITTEN-OFFER.txt`) + `LICENSE` here so the bundler includes it.

The whole app is distributed as `GPL-3.0-or-later` because it bundles GPL'd aria2.

## Browser extension

- Firefox add-on id: `minidownloader@ramazan.dev` → host manifest `allowed_extensions`.
- Chromium has two accepted IDs: unpacked builds use
  `lkllgjnnglfjifnioojkcbefjlfmfahi` (derived from `manifest.chrome.json`'s
  `key`; the private key lives in ignored `chrome-extension-key.pem`), while
  the Chrome Web Store build strips that key and uses
  `hhaobmkdgijodfieadeeanjmnneckafj`. The host manifest permits both origins
  so local development and the store release can connect.
- Build both packages: `./scripts/build-extension.sh` → `dist-ext/{firefox,chrome}`.
- The app registers the host manifest for supported browser profiles on first
  run. Extension installation remains a browser-controlled action from
  Settings → Extensions.
