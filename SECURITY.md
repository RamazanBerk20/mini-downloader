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
