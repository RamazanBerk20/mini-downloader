# Enabling the in-app auto-updater (Tauri updater)

Scaffolding note: the updater is **not wired in** by default because it requires
a signing keypair whose private half must live in CI secrets (never in the
repo). Activation is a maintainer step. macOS is intentionally out of scope.

## One-time setup

1. Generate a keypair (keep the private key secret):
   ```sh
   cd apps/desktop && cargo tauri signer generate -w ~/.tauri/minidl.key
   ```
2. Add the **private** key + its password as GitHub Actions secrets:
   `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. Put the **public** key in `apps/desktop/src-tauri/tauri.conf.json`:
   ```json
   "plugins": {
     "updater": {
       "pubkey": "<PUBLIC KEY>",
       "endpoints": ["https://github.com/RamazanBerk20/mini-downloader/releases/latest/download/latest.json"]
     }
   }
   ```

## Wire it in

The plugin is already wired behind the `updater` Cargo feature (off by default):
- `Cargo.toml` has `tauri-plugin-updater` as an optional dep + `[features] updater`.
- `lib.rs` registers the plugin under `#[cfg(feature = "updater")]`.

To activate, build with `--features updater` (add the flag to the Linux/Windows
release legs *only* if you want those binaries to self-update — leave it off for
distro/AUR builds so they update via the package manager). Then add an
update-check in the setup hook.
- `.github/workflows/release.yml`: pass `TAURI_SIGNING_PRIVATE_KEY*` env to the
  `tauri-action` step and set `args: --config '{"bundle":{"createUpdaterArtifacts":true}}'`
  (or add it to the config). tauri-action then generates + attaches `latest.json`.

## Windows code signing (separate, also maintainer-only)

Unsigned NSIS/MSI hit SmartScreen "unknown publisher". Wire an Azure Trusted
Signing (or EV cert) step into the Windows matrix leg; it needs the cert/creds
as secrets. Not required for the app to function.
