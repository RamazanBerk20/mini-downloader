# Packaging

`scripts/package-linux.sh` stages the native app, native-messaging host, desktop
entry, icon, AppStream metadata, license, and Firefox host manifest. It always
produces a portable tarball; installed `fpm` and `linuxdeploy` tools enable
deb/rpm/AppImage output. The tarball is normalized with `SOURCE_DATE_EPOCH`
(or the checked-out commit timestamp) for reproducible output.

`scripts/package-windows.ps1` expects `aria2c.exe` and `yt-dlp.exe` in
`dist-native/windows-stage`, builds the native binaries, signs them when a
certificate thumbprint is configured, creates a portable zip, and compiles the
per-user NSIS installer from `windows/installer.nsi`. The staged directory must
also contain the aria2/yt-dlp license files listed by the script. Public release
automation verifies the upstream archives, provides those files, signs only
Mini Downloader's own executables and installer, and attaches the corresponding
upstream source archives to the release.

`com.minidownloader.host.json` is the system Firefox native-messaging manifest.
The app also writes atomic, per-user Firefox and Chromium manifests on startup.
On AppImage systems it first copies the native host to a private stable data
path so browser manifests never point into a temporary AppImage mount.

The app is GPL-3.0-or-later. `THIRD_PARTY_LICENSES.txt` is generated from the
locked Cargo graph with cargo-about and must be regenerated whenever dependency
metadata changes. Distributing a bundled aria2 binary requires corresponding
GPLv2 source or a compliant written offer; the release workflow ships source.

## Browser extension IDs

- Firefox: `minidownloader@ramazan.dev`.
- Chromium unpacked development build: `lkllgjnnglfjifnioojkcbefjlfmfahi`.
- Chrome Web Store build: `hhaobmkdgijodfieadeeanjmnneckafj`.

Build both browser packages with `./scripts/build-extension.sh`.
