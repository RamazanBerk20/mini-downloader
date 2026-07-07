# Updates

Mini Downloader checks GitHub Releases on startup (and via **Settings → Updates →
Check for updates**) and compares the latest tag to the running version. No
signing key is involved — it trusts HTTPS + GitHub, the same as clicking the
download link on the releases page.

- **Windows:** the app downloads the release's `*_x64-setup.exe`, launches it,
  and quits so the NSIS installer can replace the running binary.
- **Linux (deb / rpm / AppImage / AUR):** the app never self-installs. It only
  reports that a newer release exists and opens the release page — the package
  manager (`pacman`/AUR helper, `apt`, `dnf`) performs the update. This keeps the
  install owned by the system package manager, as it should be.

Implementation: `apps/desktop/src-tauri/src/updater.rs` (`check_update`,
`install_update`). The HTTP request is made in the backend (Rust `reqwest`), so
the strict webview CSP stays intact.

## Windows code signing (optional, maintainer-only)

Unsigned NSIS/MSI installers hit SmartScreen's "unknown publisher" prompt. To
remove it, wire an Azure Trusted Signing (or EV cert) step into the Windows
matrix leg of `release.yml`; it needs the cert/credentials as GitHub secrets.
Not required for updates or the app to function.
