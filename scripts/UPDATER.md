# Updates

The native controller checks the latest GitHub Release and compares its tag to
the running Cargo package version. The implementation is
`crates/minidl-app/src/updater.rs`.

- Linux opens the trusted release page; package-manager-owned installations are
  never overwritten in place.
- Windows downloads the release's `*_x64-setup.exe`, verifies it against the
  release `SHA256SUMS`, launches it, then terminates the controller.

Only canonical URLs under this repository's HTTPS GitHub release paths are
accepted. Checksums protect transfer integrity. A configured Authenticode
certificate additionally establishes Windows publisher identity.

## Optional Windows signing

Configure both repository secrets:

- `WINDOWS_CERTIFICATE`: base64-encoded PFX/P12 bytes.
- `WINDOWS_CERTIFICATE_PASSWORD`: the PFX export password.

`scripts/setup-windows-signing.sh` validates and uploads these secrets.
Release CI imports the certificate, signs the native app, browser host, and
NSIS installer with SHA-256 plus an RFC 3161 timestamp, verifies each signature,
and removes the imported certificate. Bundled aria2 and yt-dlp binaries retain
their upstream publisher identity and are verified against pinned SHA-256
digests before packaging; they are never re-signed as Mini Downloader code.

A public Windows release requires both signing secrets. The workflow fails
closed when either is missing, so an unsigned installer cannot be published by
accident. Never commit certificates or passwords. Cloud/HSM code-signing
services should replace the import/sign step while retaining
`verify-windows-signatures.ps1`.
