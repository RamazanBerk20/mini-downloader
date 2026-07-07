# Publishing the browser extension (one-click install)

Manually side-loading is a pain: Firefox's `about:debugging` add-on is
**temporary** (wiped on restart). A permanent, one-click install needs the
extension published to the stores. The release workflow already has the signing/
publish steps wired — they stay dormant until you add the secrets below (like
Windows code signing, this needs *your* accounts). Once published, paste the
store URLs into `STORE_URLS` in `apps/desktop/src/Settings.svelte` so the app
shows "Get for Firefox / Chrome" buttons.

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
5. Cut a release — the **Sign Firefox add-on (AMO)** step runs `web-ext sign`.
6. Fill `STORE_URLS.firefox` with the AMO listing URL once approved.

## Chromium — Chrome Web Store (CWS)

1. Register at https://chrome.google.com/webstore/devconsole (one-time $5).
2. Do the first upload manually to get the **item id** (keep the same `key` from
   `extension/manifest.chrome.json` so the id — and the native-host
   `allowed_origins` — stay stable).
3. Create OAuth creds + a refresh token (see chrome-webstore-upload-cli docs).
4. Add repo secrets: `CWS_CLIENT_ID`, `CWS_CLIENT_SECRET`, `CWS_REFRESH_TOKEN`,
   `CWS_ITEM_ID`.
5. Cut a release — the **Publish Chrome extension (Web Store)** step uploads +
   auto-publishes.
6. Fill `STORE_URLS.chrome` with the Web Store URL.

## Activate IDM-style auto-install

Once published, the app can auto-install the extension into every browser (no
about:debugging, no store visit) — set the constants in
`apps/desktop/src-tauri/src/extinstall.rs`:

- `AMO_XPI_URL` → the AMO "latest signed .xpi" link, e.g.
  `https://addons.mozilla.org/firefox/downloads/latest/<slug>/latest.xpi`.
  The app drops it into every Firefox profile's `extensions/` dir (all OSes, no
  root) — works even without a Chrome account.
- `CWS_EXT_ID` → the Chrome Web Store id (needs the CWS listing). The app writes
  the Web Store external-install pointer (Linux JSON / Windows registry).

Bump to **1.2.5** when these are set (per the release plan). Firefox users then
get the add-on automatically on next launch; Chromium needs the CWS listing.

## Notes

- Both steps are `if: ${{ env.X != '' }}` gated — releases without the secrets
  still attach the unsigned zips exactly as before.
- The manifest is already AMO-submittable (MV3, `gecko.id`,
  `strict_min_version`, `data_collection_permissions: none`). Broad permissions
  (`<all_urls>`, `webRequest`, `nativeMessaging`) will draw a review but are
  declared correctly.
