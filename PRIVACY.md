# Privacy Policy — Mini Downloader Connector

_Last updated: 2026-07-07_

The **Mini Downloader Connector** browser extension is the browser half of the
open-source [Mini Downloader](https://github.com/RamazanBerk20/mini-downloader)
desktop application (GPL-3.0).

## What the extension does with your data

To capture a download and hand it to your local Mini Downloader desktop app, the
extension reads, **only for the download you initiate**:

- the download URL and response headers,
- the request headers and the site's cookies (so authenticated/session-protected
  downloads work),
- media URLs (HLS/DASH/direct) detected on the page you are viewing.

## Where that data goes

- It is sent **only to the Mini Downloader desktop application running on your own
  computer**, over the browser's native-messaging channel.
- It is **never** transmitted to the developer, to any website, or to any
  third-party server.
- The extension stores only your **settings** (enabled sites, minimum file size)
  in the browser's local storage on your device. A failed automatic handoff is
  left with the browser and is never retained or retried in the background.

## What we do NOT do

- We do **not** collect, sell, rent, or share any personal or user data.
- We do **not** use your data for advertising, profiling, creditworthiness, or
  any purpose unrelated to performing the download you asked for.
- We run no analytics and no remote code.

## Contact

Questions: open an issue at
https://github.com/RamazanBerk20/mini-downloader/issues
