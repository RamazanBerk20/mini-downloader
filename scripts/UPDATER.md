# Updates

Mini Downloader checks GitHub Releases on startup (and via **Settings → Updates →
Check for updates**) and compares the latest tag to the running version. Update
selection does not use a separate updater-manifest key: it trusts HTTPS +
GitHub and verifies the downloaded bytes against `SHA256SUMS`. When configured,
the Windows artifact's separate Authenticode signature supplies publisher
identity.

- **Windows:** the app downloads the release's `*_x64-setup.exe`, launches it,
  and quits so the NSIS installer can replace the running binary.
- **Linux (deb / rpm / AppImage / AUR):** the app never self-installs. It only
  reports that a newer release exists and opens the release page — the package
  manager (`pacman`/AUR helper, `apt`, `dnf`) performs the update. This keeps the
  install owned by the system package manager, as it should be.

Implementation: `apps/desktop/src-tauri/src/updater.rs` (`check_update`,
`install_update`). The HTTP request is made in the backend (Rust `reqwest`), so
the strict webview CSP stays intact.

## Windows code signing (optional)

Without signing credentials, the release job publishes unsigned Windows
installers and adds a SmartScreen warning to the release notes. When credentials
are configured, it imports a publicly trusted Authenticode code-signing
certificate, lets Tauri sign the app, its sidecars, and both installers, then
verifies every resulting signature. Signatures use SHA-256 and an RFC 3161
timestamp so they remain valid after the certificate expires.

Configure these GitHub Actions secrets:

- `WINDOWS_CERTIFICATE`: the raw PFX bytes encoded as one base64 string.
- `WINDOWS_CERTIFICATE_PASSWORD`: the PFX export password.

On Linux, run the repository helper and enter the PFX password when prompted:

```bash
./scripts/setup-windows-signing.sh /secure/path/code-signing-certificate.pfx
```

It validates the certificate and private key, then uploads both secrets through
the authenticated GitHub CLI. If the checkout does not have a GitHub remote,
pass `--repo OWNER/REPO`.

Or configure the secrets manually on Windows:

```powershell
$encoded = [Convert]::ToBase64String(
  [IO.File]::ReadAllBytes("C:\secure\mini-downloader-code-signing.pfx")
)
$encoded | gh secret set WINDOWS_CERTIFICATE
gh secret set WINDOWS_CERTIFICATE_PASSWORD
```

The second command prompts for the password. Never commit the PFX, its base64
form, or its password. A self-signed certificate does not establish a trusted
publisher and will not solve SmartScreen warnings; the PFX must contain a
currently valid public-trust code-signing certificate and its private key.

Modern certificate providers may keep the private key in an HSM or cloud
service instead of issuing an exportable PFX. In that case, replace
`configure-windows-signing.ps1` with the provider's Tauri `signCommand`
integration, while retaining the signature-verification step.

Local Windows builds remain unsigned unless a certificate thumbprint is added
to the Tauri configuration. Release signing is enabled only when both secrets
are present.
