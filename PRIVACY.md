# Privacy Policy — Mini Downloader and Connector

_Last updated: 2026-08-06_

Mini Downloader is an open-source desktop download manager. Its optional
browser connector hands downloads to the desktop app running on the same
computer. There is no developer-operated telemetry, analytics, advertising, or
cloud service.

For Firefox's data-consent terminology, handing information to a native
companion counts as transmission outside the add-on even when it never leaves
your computer. The connector therefore declares browsing activity (source
URLs), website content (links, media, cookies, and request/response headers),
and website activity (download actions) as required for its download-handoff
function. This data goes to the local Mini Downloader app, not to the project
developer.

## Data used to perform a download

Depending on how a download is started, the app may process:

- download, page, and referrer URLs;
- filenames, MIME types, sizes, and progress information;
- request cookies, the user-agent, and authorization/origin headers needed to
  replay an authenticated browser download;
- detected HLS, DASH, and direct-media URLs; and
- torrent metadata, peer addresses, and transfer statistics.

The connector scopes cookies to the URL being handed off. It sends a capture
only through the browser's native-messaging channel to the local Mini
Downloader host. Failed handoffs are not queued for a later retry. Detected
media URLs are held in browser session storage (or memory) and are cleared on
navigation, tab close, or browser exit. Browser local storage contains only the
connector's settings.

The connector does not capture, retain, or hand off data from private/incognito
tabs or downloads, even if the browser is configured to let the extension run
in private browsing.

## Local storage and retention

The desktop app stores its queue and settings in its private per-user data
directory. Download history retains URLs, filenames, destination paths,
timestamps, status, and transfer metadata until you remove the record. Cookies
and extra authorization headers are retained only while needed to resume or
retry a transfer and are erased when it completes or when its record is
deleted.

aria2 may keep a private resume-session file containing URLs and replay headers
for unfinished transfers. BitTorrent DHT state may also be stored locally. On
Linux, completed files can receive `user.xdg.origin.url` and
`user.xdg.referrer.url` extended attributes so the desktop can identify their
origin. Removing a history record does not delete those attributes from a file
you keep.

The app's data directory is restricted to the current operating-system user on
supported Unix systems. Anyone who can access your operating-system account can
still read that account's application data; use full-disk encryption and a
locked session where this matters.

## Network connections

Downloads contact the source servers you select. Torrents can expose your IP
address to trackers and peers; DHT, peer exchange, and local peer discovery can
be disabled in Settings. yt-dlp and aria2 run locally but contact download
sources as required. If you configure a proxy, download requests are routed
through that proxy; its operator may be able to observe the destination and,
for unencrypted protocols, request data.

When local-address blocking is enabled, the app rejects a target, and every
redirect hop it follows itself, that currently resolves to a private, loopback,
link-local, or reserved address. aria2 does not follow redirects for transfers
that carry browser-captured credentials. This is defense in depth, not a
network sandbox: DNS can change between validation and connection, and yt-dlp
may follow redirects internally. Do not use the downloader as a security
boundary on an untrusted network.

An explicit update check contacts GitHub's public API. Installing an update on
Windows downloads the signed installer and checksum manifest from this
project's GitHub Releases page. No unique identifier or usage history is sent.

## What the project operator does not do

- The project operator does not receive, sell, rent, share, or profile user
  data.
- We do not use remote code or a developer-operated relay.
- We do not automatically replay failed captures later.
- We do not transmit credentials anywhere except to the download source that
  requires them and locally between the connector and desktop app.

## Contact

Ordinary privacy questions can be filed at
<https://github.com/RamazanBerk20/mini-downloader/issues>. Send sensitive
reports privately using the process in [SECURITY.md](SECURITY.md).
