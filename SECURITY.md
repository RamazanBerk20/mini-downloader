# Security policy

## Supported releases

Security fixes are provided for the newest stable Mini Downloader release.
Users should update to the latest patch release before reporting an issue.
Development snapshots and the archived Tauri/Svelte shell are not supported.

## Report a vulnerability privately

Do not open a public issue for a vulnerability or include secrets, cookies,
tokens, private URLs, or downloaded files in a public report.

Use [GitHub's private vulnerability report form](https://github.com/RamazanBerk20/mini-downloader/security/advisories/new).
If that form is unavailable, email `ramazanberksirin@protonmail.com` with the
subject `Mini Downloader security report`.

Include the affected version and platform, the impact, reproducible steps, and
the smallest safe proof of concept you can provide. Remove personal data and
rotate any credential that may have been exposed.

The maintainer aims to acknowledge reports within three business days, provide
an initial assessment within seven days, and coordinate a fix and disclosure
timeline based on severity. Please allow time for a release before publishing
details.

## Scope

Reports about URL ingestion, browser/native messaging, local IPC, updater or
package authenticity, filesystem access, command execution, or leakage of
cookies and credentials are in scope. Vulnerabilities in aria2, yt-dlp,
browsers, or operating-system components should also be reported upstream;
include Mini Downloader when its integration makes the issue exploitable.

## Known limits

- **Private-address blocking and DNS rebinding.** With "Block private
  addresses" on, the app's own redirect probe (the only request that carries
  captured cookies before aria2 starts) connects only to addresses its
  resolver has checked, so the checked address is the one connected to.
  aria2 cannot be pinned to an address: it has no `--resolve`-style option,
  and rewriting the URL to an IP literal breaks TLS name checks. aria2
  therefore resolves the final host again when it downloads, with redirects
  disabled. A host whose DNS answer changes between the check and aria2's
  lookup can still make aria2 download from a private address, sending it
  the credentials captured for that same host name. With a proxy configured,
  the proxy resolves every name and the app can only check names before each
  request. With the setting off (the default), private addresses are allowed
  on purpose.
