// Mini Downloader Connector — background/event page.
// Path A (primary): Firefox response-header sniffing preserves full request
// fidelity, then cancels only after the native app acknowledges the job.
// Path B (fallback): the browser starts its download first; it is cancelled and
// erased only after the native app acknowledges the same job.
// Both forward a CaptureJob to the native host over native messaging.

const b = globalThis.browser || globalThis.chrome;
const HOST = "com.minidownloader.host";
const t = (k) => b.i18n.getMessage(k);

// Firefox exposes `browser`; Chromium exposes only `chrome`. Firefox MV3 keeps
// blocking webRequest (Path A: sniff headers + cancel → full cookie fidelity);
// Chromium MV3 removed it, so there we rely on Path B (downloads.onCreated).
const IS_FIREFOX = typeof globalThis.browser !== "undefined";
// The native host also derives this from the browser-provided launch argument
// for our known store IDs. Keep it in every connector message as a fallback
// for Firefox/Chromium forks with a different invocation shape.
const BROWSER_FAMILY = IS_FIREFOX ? "firefox" : "chromium";

const DEFAULTS = {
  enabled: true,
  minSize: 1048576, // 1 MiB floor: ignore tiny/inline files
  disabledHosts: [],
  blacklistExts: [], // file types the browser keeps (lowercase, no dot)
  blacklistMagnet: false, // leave magnet: links to the system handler
};
let settings = { ...DEFAULTS };

async function loadSettings() {
  try {
    const s = await b.storage.local.get("settings");
    settings = { ...DEFAULTS, ...(s.settings || {}) };
  } catch {}
}
// A shared promise the capture handlers await before reading `settings`. On an
// MV3 cold start the event that wakes the worker can fire before the un-awaited
// load resolves, so without this the first download after each wake is judged
// against DEFAULTS (ignoring enabled/disabledHosts/minSize).
let settingsReady = loadSettings();
b.storage.onChanged.addListener((c) => {
  if (c.settings) settings = { ...DEFAULTS, ...(c.settings.newValue || {}) };
});

function hostOf(url) {
  try {
    return new URL(url).hostname;
  } catch {
    return "";
  }
}
function enabledForUrl(url) {
  return settings.enabled && !settings.disabledHosts.includes(hostOf(url));
}

function notify(title, message) {
  try {
    b.notifications.create({
      type: "basic",
      iconUrl: b.runtime.getURL("icons/96.png"),
      title,
      message: message || "",
    });
  } catch {}
}

async function sendJob(job) {
  try {
    const reply = await b.runtime.sendNativeMessage(HOST, {
      ...job,
      browserFamily: BROWSER_FAMILY,
    });
    if (reply && reply.ok) notify(t("notifySentTitle"), job.filename || job.url);
    else notify(t("notifyRejectedTitle"), (reply && reply.error) || t("notifyUnknownError"));
    return reply;
  } catch (e) {
    notify(t("notifyNotReachableTitle"), t("notifyNotReachableBody"));
    // Automatic captures keep the browser's download when this happens. Do not
    // save the job for a later automatic retry: URLs and auth can be stale, and
    // replaying it later would start a download the user did not request then.
    return null;
  }
}

// Presence is intentionally silent: it confirms an already-running desktop
// app, but does not notify, queue work, or cause the native host to launch the
// app just because a browser background worker woke up.
async function sendPresence() {
  try {
    await b.runtime.sendNativeMessage(HOST, {
      presence: true,
      browserFamily: BROWSER_FAMILY,
    });
  } catch {}
}

// Older connector releases stored failed captures in this key and replayed
// them from alarms/startup. Discard that legacy queue during upgrade so no old
// capture can silently start later. New failures are left in the browser (or,
// for an explicit context-menu action, reported immediately to the user).
const LEGACY_RETRY_KEY = "pendingJobs";
async function discardLegacyRetries() {
  try {
    await b.storage.local.remove(LEGACY_RETRY_KEY);
  } catch {}
}

b.runtime.onStartup.addListener(() => {
  void sendPresence();
});
b.runtime.onInstalled.addListener(() => {
  void discardLegacyRetries();
  void sendPresence();
});
try {
  b.alarms.create("ldm-presence", { periodInMinutes: 5 });
  b.alarms.onAlarm.addListener((a) => {
    if (a.name === "ldm-presence") void sendPresence();
  });
} catch {}
void discardLegacyRetries();
void sendPresence();

// Dedup the same URL across Path A / Path B within a short window.
const recent = new Map();
function seen(url) {
  const now = Date.now();
  for (const [k, t] of recent) if (now - t > 5000) recent.delete(k);
  if (recent.has(url)) return true;
  recent.set(url, now);
  return false;
}

// ---------- Path A: webRequest (Firefox only) ----------

if (IS_FIREFOX) {
const reqHeaders = new Map(); // requestId -> headers[]
b.webRequest.onBeforeSendHeaders.addListener(
  (d) => {
    reqHeaders.set(d.requestId, d.requestHeaders || []);
  },
  { urls: ["<all_urls>"] },
  ["requestHeaders"],
);
const evict = (d) => reqHeaders.delete(d.requestId);
b.webRequest.onCompleted.addListener(evict, { urls: ["<all_urls>"] });
b.webRequest.onErrorOccurred.addListener(evict, { urls: ["<all_urls>"] });

const DOWNLOADABLE_CT = new Set([
  "application/octet-stream",
  "application/zip",
  "application/x-7z-compressed",
  "application/x-rar-compressed",
  "application/x-tar",
  "application/gzip",
  "application/x-xz",
  "application/x-bzip2",
  "application/x-zstd",
  "application/x-msdownload",
  "application/vnd.debian.binary-package",
  "application/x-rpm",
  "application/x-iso9660-image",
  "application/x-apple-diskimage",
  "application/vnd.android.package-archive",
]);
const SKIP_CT_PREFIX = [
  "text/html",
  "text/css",
  "application/javascript",
  "text/javascript",
  "application/json",
  "image/",
  "text/plain",
  "application/xml",
  "text/xml",
];
const FILE_EXT_RE =
  /\.(zip|7z|rar|tar|gz|tgz|xz|bz2|zst|iso|img|dmg|exe|msi|deb|rpm|appimage|pkg|apk|mp4|mkv|webm|avi|mov|flv|mp3|flac|m4a|wav|ogg|opus|pdf|epub|bin|run)(\?|$)/i;

function filenameFromCD(cd, url) {
  if (cd) {
    const m =
      /filename\*=(?:UTF-8'')?["']?([^;"']+)/i.exec(cd) ||
      /filename=["']?([^;"']+)/i.exec(cd);
    if (m) {
      try {
        return decodeURIComponent(m[1]);
      } catch {
        return m[1];
      }
    }
  }
  try {
    const n = new URL(url).pathname.split("/").pop();
    if (n) return decodeURIComponent(n);
  } catch {}
  return undefined;
}

// Lowercased trailing extension of a filename or URL path ("" when none).
function extOf(name) {
  const m = /\.([a-z0-9]{1,8})$/i.exec((name || "").split(/[?#]/)[0]);
  return m ? m[1].toLowerCase() : "";
}

// User blacklist: these file types stay with the browser's own download flow.
function isBlacklisted(url, filename) {
  const list = settings.blacklistExts || [];
  if (!list.length) return false;
  let fromUrl = "";
  try {
    fromUrl = extOf(new URL(url).pathname);
  } catch {}
  const fromName = extOf(filename);
  return (fromName && list.includes(fromName)) || (fromUrl && list.includes(fromUrl));
}

function shouldHijack(cd, ct, len, url) {
  if (/^blob:|^data:/i.test(url)) return false;
  if (cd && /attachment/i.test(cd)) return true;
  const lct = (ct || "").toLowerCase().split(";")[0].trim();
  if (SKIP_CT_PREFIX.some((p) => lct.startsWith(p))) return false;
  const bigEnough = len < 0 || len >= settings.minSize;
  if (DOWNLOADABLE_CT.has(lct)) return bigEnough;
  if (FILE_EXT_RE.test(url)) return bigEnough;
  return false;
}

function jobFromRequest(url, filename, mime, size, headersArr, cookieStoreId) {
  let cookie, referrer, userAgent;
  const extra = [];
  for (const x of headersArr) {
    const n = x.name.toLowerCase();
    if (n === "cookie") cookie = x.value;
    else if (n === "referer") referrer = x.value;
    else if (n === "user-agent") userAgent = x.value;
    else if (n === "authorization" || n === "origin") extra.push([x.name, x.value]);
  }
  return {
    url,
    filename,
    mime,
    size,
    referrer,
    user_agent: userAgent,
    cookie,
    extra_headers: extra,
    kind: url.startsWith("magnet:") ? "magnet" : "http",
    cookie_store_id: cookieStoreId,
  };
}

b.webRequest.onHeadersReceived.addListener(
  async (d) => {
    // Firefox honors a Promise return from a blocking listener; await the initial
    // settings load so a cold-start request isn't judged against DEFAULTS.
    await settingsReady;
    if (!enabledForUrl(d.url)) return {};
    const h = {};
    for (const x of d.responseHeaders || []) h[x.name.toLowerCase()] = x.value;
    const cd = h["content-disposition"] || "";
    const ct = h["content-type"] || "";
    const lenRaw = parseInt(h["content-length"] || "-1", 10);
    const len = Number.isNaN(lenRaw) ? -1 : lenRaw;
    if (!shouldHijack(cd, ct, len, d.url)) return {};
    if (isBlacklisted(d.url, filenameFromCD(cd, d.url))) return {};
    // A duplicate must never be cancelled merely because another request used
    // the same URL. Only the request whose native handoff is acknowledged may
    // be cancelled below.
    if (seen(d.url)) return {};

    const job = jobFromRequest(
      d.url,
      filenameFromCD(cd, d.url),
      (ct.split(";")[0] || "").trim(),
      len,
      reqHeaders.get(d.requestId) || [],
      d.cookieStoreId,
    );
    // Firefox lets a blocking webRequest listener return a Promise. Keep the
    // browser request alive while the native host appraises the capture; a
    // failed/rejected handoff falls through to Firefox's normal download.
    const reply = await sendJob(job);
    return reply && reply.ok ? { cancel: true } : {};
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] },
  ["blocking", "responseHeaders"],
);
} // end Path A (Firefox only)

// ---------- Path B: downloads.onCreated fallback (Firefox + Chromium) ----------

b.downloads.onCreated.addListener(async (item) => {
  try {
    if (item.byExtensionId === b.runtime.id) return;
    if (/^blob:|^data:/i.test(item.url)) return;
    await settingsReady;
    if (!enabledForUrl(item.url)) return;
    if (item.url.startsWith("magnet:") && settings.blacklistMagnet) return;
    if (isBlacklisted(item.url, item.filename ? item.filename.split("/").pop() : "")) return;
    if (item.totalBytes > -1 && item.totalBytes < settings.minSize) return;
    if (seen(item.url)) return;

    let cookie = "";
    try {
      const cs = await b.cookies.getAll({ url: item.url, storeId: item.cookieStoreId });
      cookie = cs.map((c) => `${c.name}=${c.value}`).join("; ");
    } catch {}

    const reply = await sendJob({
      url: item.url,
      filename: item.filename ? item.filename.split("/").pop() : undefined,
      mime: item.mime,
      size: item.totalBytes,
      referrer: item.referrer || undefined,
      cookie: cookie || undefined,
      extra_headers: [],
      kind: item.url.startsWith("magnet:") ? "magnet" : "http",
      cookie_store_id: item.cookieStoreId,
    });
    // Only pull the download out of the browser once the app has accepted it.
    // If the app is down/unreachable or rejects the job, leave the browser's own
    // download intact instead of erasing it with no fallback.
    if (reply && reply.ok) {
      await b.downloads.cancel(item.id).catch(() => {});
      await b.downloads.erase({ id: item.id }).catch(() => {});
    }
  } catch {}
});

// ---------- Context menu ----------

function createMenu() {
  try {
    b.contextMenus.removeAll(() => {
      b.contextMenus.create({
        id: "ldm-download",
        title: t("actionTitle"),
        contexts: ["link", "video", "audio", "image", "selection"],
      });
      b.contextMenus.create({
        id: "ldm-links",
        title: "Mini Downloader: all links on page",
        contexts: ["page"],
      });
      b.contextMenus.create({
        id: "ldm-images",
        title: "Mini Downloader: all images on page",
        contexts: ["page"],
      });
    });
  } catch {}
}
b.runtime.onInstalled.addListener(createMenu);
b.runtime.onStartup.addListener(createMenu);
createMenu();

b.contextMenus.onClicked.addListener(async (info, tab) => {
  // Bulk grab: harvest every downloadable link / image on the page.
  if (info.menuItemId === "ldm-links" || info.menuItemId === "ldm-images") {
    if (!tab) return;
    const what = info.menuItemId === "ldm-images" ? "images" : "links";
    let urls = [];
    try {
      urls = (await b.tabs.sendMessage(tab.id, { type: "ldm-harvest", what })) || [];
    } catch {}
    urls = urls.slice(0, 200); // cap a runaway page
    let cookie = "";
    try {
      const cs = await b.cookies.getAll({ url: tab.url });
      cookie = cs.map((c) => `${c.name}=${c.value}`).join("; ");
    } catch {}
    // Group the whole harvest into one package on the app side. Only batches of
    // 2+ get a batch id — a single hit lands ungrouped like a normal capture.
    const batchId = urls.length >= 2 && crypto.randomUUID ? crypto.randomUUID() : undefined;
    let batchName;
    if (batchId) {
      try {
        batchName = (tab.title || "").trim() || new URL(tab.url).hostname;
      } catch {
        batchName = undefined;
      }
    }
    let sent = 0;
    for (const u of urls) {
      const reply = await sendJob({
        url: u,
        referrer: tab.url,
        cookie: cookie || undefined,
        extra_headers: [],
        kind: "http",
        batch_id: batchId,
        batch_name: batchName,
      });
      if (reply && reply.ok) sent++;
    }
    notify(t("notifySentTitle"), `${sent} / ${urls.length}`);
    return;
  }

  const url = info.linkUrl || info.srcUrl || (info.selectionText || "").trim();
  if (!url || !/^(https?:|ftp:|magnet:)/i.test(url)) return;
  let cookie = "";
  try {
    const cs = await b.cookies.getAll({ url });
    cookie = cs.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {}
  sendJob({
    url,
    referrer: tab && tab.url,
    cookie: cookie || undefined,
    extra_headers: [],
    kind: url.startsWith("magnet:") ? "magnet" : "http",
  });
});

// ---------- Media sniffer (HLS/DASH) + content-script media ----------

// Per-tab sniffed media is persisted in `storage.session` (falling back to
// `storage.local`) so it survives the MV3 worker/event-page being suspended —
// an in-memory Map is lost after ~30s idle, leaving the popup empty. Cleared on
// tab close / navigation.
const mediaStore = (b.storage && b.storage.session) || b.storage.local;
const MEDIA_KEY = (tabId) => `media:${tabId}`;

async function getMedia(tabId) {
  try {
    const key = MEDIA_KEY(tabId);
    const r = await mediaStore.get(key);
    return r[key] || [];
  } catch {
    return [];
  }
}
async function addMedia(tabId, entry) {
  if (tabId === undefined || tabId < 0) return;
  const list = await getMedia(tabId);
  if (list.some((m) => m.url === entry.url)) return;
  list.push(entry);
  try {
    await mediaStore.set({ [MEDIA_KEY(tabId)]: list });
  } catch {}
}
function clearMedia(tabId) {
  try {
    mediaStore.remove(MEDIA_KEY(tabId));
  } catch {}
}

b.webRequest.onResponseStarted.addListener(
  (d) => {
    const isHls = /\.m3u8(\?|$)|mpegurl/i.test(d.url);
    const isDash = /\.mpd(\?|$)|dash\+xml/i.test(d.url);
    if (!isHls && !isDash) return;
    addMedia(d.tabId, { url: d.url, type: isDash ? "dash" : "hls" });
  },
  { urls: ["<all_urls>"], types: ["xmlhttprequest", "media", "other"] },
);
b.tabs.onRemoved.addListener((id) => clearMedia(id));
b.tabs.onUpdated.addListener((id, ch) => {
  if (ch.url) clearMedia(id);
});

b.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg) return;
  if (msg.type === "ldm-media" && sender.tab) {
    const tabId = sender.tab.id;
    for (const u of msg.urls || []) addMedia(tabId, { url: u, type: "file" });
    return;
  }
  if (msg.type === "ldm-get-media") {
    // Firefox honors a returned Promise; Chromium needs sendResponse + `return
    // true` to keep the message channel open for the async storage read.
    const p = getMedia(msg.tabId);
    if (IS_FIREFOX) return p;
    p.then((media) => sendResponse(media));
    return true;
  }
  if (msg.type === "ldm-grab") {
    const p = sendJob(msg.job);
    if (IS_FIREFOX) return p;
    p.then((reply) => sendResponse(reply));
    return true;
  }
});
