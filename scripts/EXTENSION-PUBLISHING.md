# Publishing the browser extension

The extension is available from the public stores:

- Firefox: https://addons.mozilla.org/firefox/addon/mini-downloader-connector/
- Chrome: https://chromewebstore.google.com/detail/mini-downloader-connector/hhaobmkdgijodfieadeeanjmnneckafj

The desktop app links to those listings from Settings → Extensions.
No runtime key, AMO token, Chrome OAuth token, or extension private key is
needed for users to install the published extension.

The connector sends a silent, local presence signal while the desktop app is
running so the app can show an accurate setup prompt. Any connector change
needs a version bump in both extension manifests and a new AMO/CWS submission;
the desktop app uses active connector metadata in supported browser profiles as
a local fallback, but only a native-messaging heartbeat proves a live bridge.

## Firefox — addons.mozilla.org (AMO)

1. Create a free account at https://addons.mozilla.org and register the add-on
   (id `minidownloader@ramazan.dev`, already in the manifest).
2. Generate an API key: AMO → Tools → **Manage API Keys** → JWT issuer + secret.
3. Add GitHub repo secrets: `AMO_API_KEY`, `AMO_API_SECRET`.
4. (Optional) Repo **variable** `AMO_CHANNEL`:
   - `listed` (default) — submits to the store; auto-lists after review.
   - `unlisted` — signs a self-distributable `.xpi` that the workflow attaches to
     the release; users install it permanently by opening the file. No review,
     no store page. Good as an immediate stopgap.
5. Cut a release whose tag matches the extension manifest version — the
   **Sign Firefox add-on (AMO)** step runs `web-ext sign`.

## Chromium — Chrome Web Store (CWS)

1. Register at https://chrome.google.com/webstore/devconsole (one-time $5).
2. Do the first upload manually to get the **item id** (keep the same `key` from
   `extension/manifest.chrome.json` so the id — and the native-host
   `allowed_origins` — stay stable).
3. Create OAuth creds + a refresh token (see chrome-webstore-upload-cli docs).
4. Add repo secrets: `CWS_CLIENT_ID`, `CWS_CLIENT_SECRET`, `CWS_REFRESH_TOKEN`,
   `CWS_ITEM_ID`.
5. Cut a release whose tag matches the extension manifest version — the
   **Publish Chrome extension (Web Store)** step uploads + auto-publishes.

## Installation behavior

There is no credential that enables a public desktop app to silently install
extensions in consumer browsers. Firefox requires an explicit user install for
desktop companion apps. Chrome's external-install flow still requires user
confirmation on Windows and macOS. Do not ship publisher credentials in the
app or ask users for them.

Fully unattended installation is available only for managed enterprise
deployments. Administrators can deploy the existing public artifacts through
their browser policies:

- Firefox add-on ID: `minidownloader@ramazan.dev`; AMO XPI URL:
  `https://addons.mozilla.org/firefox/downloads/latest/minidownloader@ramazan.dev/latest.xpi`
- Chrome extension ID: `hhaobmkdgijodfieadeeanjmnneckafj`; update URL:
  `https://clients2.google.com/service/update2/crx`

## Notes

- Both steps are `if: ${{ env.X != '' }}` gated — releases without the secrets
  still attach the unsigned zips exactly as before.
- The manifest is already AMO-submittable (MV3, `gecko.id`,
  `strict_min_version`, `data_collection_permissions: none`). Broad permissions
  (`<all_urls>`, `webRequest`, `nativeMessaging`) will draw a review but are
  declared correctly.
