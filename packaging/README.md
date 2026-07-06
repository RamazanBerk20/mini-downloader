# Packaging

- `com.minidownloader.host.json` — Firefox native-messaging host manifest. For `.deb`/`.rpm`
  the post-install script writes it (with the real host path) to
  `/usr/lib/mozilla/native-messaging-hosts/`. For AppImage the app writes a
  per-user copy to `~/.mozilla/native-messaging-hosts/` on first run (see
  `install_browser_integration`).
- `aria2/` — aria2 is GPLv2; distributing the `aria2c` binary requires shipping
  the corresponding source or a written offer. Place the pinned aria2 source
  tarball (or `WRITTEN-OFFER.txt`) + `LICENSE` here so the bundler includes it.

The whole app is distributed as `GPL-3.0-or-later` because it bundles GPL'd aria2.

## Browser extension

- Firefox add-on id: `minidownloader@ramazan.dev` → host manifest `allowed_extensions`.
- Chromium extension id: `lkllgjnnglfjifnioojkcbefjlfmfahi` (from `manifest.chrome.json`'s
  `key`; private key in `chrome-extension-key.pem`, keep it secret / out of releases)
  → host manifest `allowed_origins: ["chrome-extension://<id>/"]`.
- Build both packages: `./scripts/build-extension.sh` → `dist-ext/{firefox,chrome}`.
- The app installs the host manifest for all detected browsers on first run and via
  Settings → "Install native-messaging host".
