// Mini Downloader Connector — background/event page.
// Path A (primary): Firefox response-header sniffing preserves full request
// fidelity, then cancels only after the native app acknowledges the job.
// Path B (fallback): the browser starts its download first; it is cancelled and
// erased only after the native app acknowledges the same job.
// Both forward a CaptureJob to the native host over native messaging.

if (typeof importScripts === "function" && !globalThis.ldmBackgroundHelpers) {
  importScripts("background-helpers.js");
}

const b = globalThis.browser || globalThis.chrome;
const helpers = globalThis.ldmBackgroundHelpers;
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

function enabledForUrl(url) {
  return settings.enabled && !helpers.isHostDisabled(url, settings.disabledHosts);
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

async function sendJob(job, { notifyResult = true } = {}) {
  try {
    const reply = await b.runtime.sendNativeMessage(HOST, {
      ...job,
      browserFamily: BROWSER_FAMILY,
    });
    if (notifyResult) {
      if (reply && reply.ok) notify(t("notifySentTitle"), job.filename || job.url);
      else notify(t("notifyRejectedTitle"), (reply && reply.error) || t("notifyUnknownError"));
    }
    return reply;
  } catch (e) {
    if (notifyResult) notify(t("notifyNotReachableTitle"), t("notifyNotReachableBody"));
    // Automatic captures keep the browser's download when this happens. Do not
    // save the job for a later automatic retry: URLs and auth can be stale, and
    // replaying it later would start a download the user did not request then.
    return null;
  }
}

async function enrichJob(job, tab) {
  return helpers.enrichCaptureJob(job, tab, async (query) => {
    try {
      return await b.cookies.getAll(query);
    } catch {
      return [];
    }
  });
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

async function discardPersistedMedia() {
  try {
    const stored = await b.storage.local.get(null);
    const keys = Object.keys(stored).filter((key) => key.startsWith("media:"));
    if (keys.length) await b.storage.local.remove(keys);
  } catch {}
}

b.runtime.onStartup.addListener(() => {
  void sendPresence();
});
b.runtime.onInstalled.addListener(() => {
  void discardLegacyRetries();
  void discardPersistedMedia();
  void sendPresence();
});
try {
  b.alarms.create("ldm-presence", { periodInMinutes: 5 });
  b.alarms.onAlarm.addListener((a) => {
    if (a.name === "ldm-presence") void sendPresence();
  });
} catch {}
void discardLegacyRetries();
void discardPersistedMedia();
void sendPresence();

// Correlate Firefox's Path A handoff with the Path B download event it may
// produce for the same browser request. Every Path A request still reaches the
// app, where the user's duplicate-prevention setting is authoritative.
const pathAHandoffs = new Map();
function rememberPathAHandoff(url, handoff) {
  const at = Date.now();
  pathAHandoffs.set(url, { at, handoff });
  while (pathAHandoffs.size > 512) {
    pathAHandoffs.delete(pathAHandoffs.keys().next().value);
  }
  setTimeout(() => {
    if (pathAHandoffs.get(url)?.at === at) pathAHandoffs.delete(url);
  }, 5000);
}
function recentPathAHandoff(url) {
  const now = Date.now();
  for (const [key, entry] of pathAHandoffs) {
    if (now - entry.at > 5000) pathAHandoffs.delete(key);
  }
  return pathAHandoffs.get(url)?.handoff || null;
}

// ---------- Path A: webRequest (Firefox only) ----------

if (IS_FIREFOX) {
const reqHeaders = new Map(); // requestId -> headers[]
b.webRequest.onBeforeSendHeaders.addListener(
  (d) => {
    if (d.incognito) return;
    const wanted = new Set(["cookie", "referer", "user-agent", "authorization", "origin"]);
    let bytes = 0;
    const headers = [];
    for (const header of d.requestHeaders || []) {
      if (typeof header.name !== "string" || typeof header.value !== "string") continue;
      if (!wanted.has(header.name.toLowerCase())) continue;
      const size = header.name.length + header.value.length;
      if (bytes + size > 64 * 1024) break;
      bytes += size;
      headers.push(header);
    }
    reqHeaders.set(d.requestId, headers);
    while (reqHeaders.size > 512) reqHeaders.delete(reqHeaders.keys().next().value);
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
    if (d.incognito) return {};
    if (!enabledForUrl(d.url)) return {};
    const h = {};
    for (const x of d.responseHeaders || []) h[x.name.toLowerCase()] = x.value;
    const cd = h["content-disposition"] || "";
    const ct = h["content-type"] || "";
    const lenRaw = parseInt(h["content-length"] || "-1", 10);
    const len = Number.isNaN(lenRaw) ? -1 : lenRaw;
    if (!shouldHijack(cd, ct, len, d.url)) return {};
    if (isBlacklisted(d.url, filenameFromCD(cd, d.url))) return {};
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
    const handoff = sendJob(job);
    rememberPathAHandoff(d.url, handoff);
    const reply = await handoff;
    return reply && reply.ok ? { cancel: true } : {};
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] },
  ["blocking", "responseHeaders"],
);
} // end Path A (Firefox only)

// ---------- Path B: downloads.onCreated fallback (Firefox + Chromium) ----------

b.downloads.onCreated.addListener(async (item) => {
  try {
    if (item.incognito) return;
    if (item.byExtensionId === b.runtime.id) return;
    if (/^blob:|^data:/i.test(item.url)) return;
    await settingsReady;
    if (!enabledForUrl(item.url)) return;
    // Ordinary magnet clicks belong to the system-selected handler. The
    // context-menu action below remains an explicit Mini Downloader override.
    if (item.url.startsWith("magnet:")) return;
    if (isBlacklisted(item.url, item.filename ? item.filename.split(/[\\/]/).pop() : "")) return;
    if (item.totalBytes > -1 && item.totalBytes < settings.minSize) return;

    // Firefox can surface the same request through both capture paths. Await
    // Path A's in-flight decision: on acceptance, remove this browser copy; on
    // failure, let Path B make its own handoff so the normal fallback survives.
    const earlierHandoff = recentPathAHandoff(item.url);
    if (earlierHandoff) {
      const earlierReply = await earlierHandoff.catch(() => null);
      if (earlierReply && earlierReply.ok) {
        await b.downloads.cancel(item.id).catch(() => {});
        await b.downloads.erase({ id: item.id }).catch(() => {});
        return;
      }
    }

    const job = await enrichJob({
      url: item.url,
      filename: item.filename ? item.filename.split(/[\\/]/).pop() : undefined,
      mime: item.mime,
      size: item.totalBytes,
      referrer: item.referrer || undefined,
      extra_headers: [],
      kind: item.url.startsWith("magnet:") ? "magnet" : "http",
      cookie_store_id: item.cookieStoreId,
    }, { cookieStoreId: item.cookieStoreId });
    const reply = await sendJob(job);
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

async function createMenu() {
  try {
    await b.contextMenus.removeAll();
    await Promise.resolve(b.contextMenus.create({
      id: "ldm-download",
      title: t("actionTitle"),
      contexts: ["link", "video", "audio", "image", "selection"],
    }));
    await Promise.resolve(b.contextMenus.create({
      id: "ldm-links",
      title: t("menuAllLinks"),
      contexts: ["page"],
    }));
    await Promise.resolve(b.contextMenus.create({
      id: "ldm-images",
      title: t("menuAllImages"),
      contexts: ["page"],
    }));
  } catch {}
}
b.runtime.onInstalled.addListener(() => void createMenu());
b.runtime.onStartup.addListener(() => void createMenu());
void createMenu();

b.contextMenus.onClicked.addListener(async (info, tab) => {
  if (tab && tab.incognito) return;
  // Bulk grab: harvest every downloadable link / image on the page.
  if (info.menuItemId === "ldm-links" || info.menuItemId === "ldm-images") {
    if (!tab) return;
    const what = info.menuItemId === "ldm-images" ? "images" : "links";
    let urls = [];
    try {
      urls = (await b.tabs.sendMessage(tab.id, { type: "ldm-harvest", what })) || [];
    } catch {}
    urls = urls.slice(0, 200); // cap a runaway page
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
      const job = await enrichJob({
        url: u,
        referrer: tab.url,
        extra_headers: [],
        kind: "http",
        batch_id: batchId,
        batch_name: batchName,
      }, tab);
      const reply = await sendJob(job, { notifyResult: false });
      if (reply && reply.ok) sent++;
    }
    notify(t("notifySentTitle"), `${sent} / ${urls.length}`);
    return;
  }

  const url = info.linkUrl || info.srcUrl || (info.selectionText || "").trim();
  if (!url || !/^(https?:|ftp:|magnet:)/i.test(url)) return;
  const job = await enrichJob({
    url,
    referrer: tab && tab.url,
    extra_headers: [],
    kind: url.startsWith("magnet:") ? "magnet" : "http",
  }, tab);
  void sendJob(job);
});

// ---------- Media sniffer (HLS/DASH) + content-script media ----------

// Per-tab media lives only for the browser session. Never fall back to
// persistent local storage: signed media URLs can contain reusable credentials.
const mediaStore = b.storage && b.storage.session;
const memoryMedia = new Map();
const mediaOperations = new Map();
const MEDIA_KEY = (tabId) => `media:${tabId}`;

async function readMedia(tabId) {
  if (!mediaStore) return memoryMedia.get(tabId) || [];
  try {
    const key = MEDIA_KEY(tabId);
    const r = await mediaStore.get(key);
    return r[key] || [];
  } catch {
    return [];
  }
}

function enqueueMedia(tabId, operation) {
  const previous = mediaOperations.get(tabId) || Promise.resolve();
  const current = previous
    .catch(() => {})
    .then(operation)
    .finally(() => {
      if (mediaOperations.get(tabId) === current) mediaOperations.delete(tabId);
    });
  mediaOperations.set(tabId, current);
  return current;
}

async function getMedia(tabId) {
  await (mediaOperations.get(tabId) || Promise.resolve()).catch(() => {});
  return readMedia(tabId);
}

function addMedia(tabId, entry) {
  if (tabId === undefined || tabId < 0) return;
  return enqueueMedia(tabId, async () => {
    const list = await readMedia(tabId);
    const next = helpers.addBoundedMedia(list, entry);
    if (next.length === list.length && next.every((item, index) => item === list[index])) return;
    if (!mediaStore) {
      memoryMedia.set(tabId, next);
      return;
    }
    try {
      await mediaStore.set({ [MEDIA_KEY(tabId)]: next });
    } catch {}
  });
}
function clearMedia(tabId) {
  return enqueueMedia(tabId, async () => {
    memoryMedia.delete(tabId);
    if (!mediaStore) return;
    try {
      await mediaStore.remove(MEDIA_KEY(tabId));
    } catch {}
  });
}

b.webRequest.onResponseStarted.addListener(
  (d) => {
    if (d.incognito) return;
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
    if (sender.tab.incognito) return;
    const tabId = sender.tab.id;
    for (const u of msg.urls || []) addMedia(tabId, { url: u, type: "file" });
    return;
  }
  if (msg.type === "ldm-get-media") {
    if (msg.private || (sender.tab && sender.tab.incognito)) {
      if (IS_FIREFOX) return Promise.resolve([]);
      sendResponse([]);
      return false;
    }
    // Firefox honors a returned Promise; Chromium needs sendResponse + `return
    // true` to keep the message channel open for the async storage read.
    const p = getMedia(msg.tabId);
    if (IS_FIREFOX) return p;
    p.then((media) => sendResponse(media));
    return true;
  }
  if (msg.type === "ldm-grab") {
    if (msg.private || (sender.tab && sender.tab.incognito)) {
      if (IS_FIREFOX) return Promise.resolve(null);
      sendResponse(null);
      return false;
    }
    const p = enrichJob(msg.job, sender.tab).then((job) => sendJob(job));
    if (IS_FIREFOX) return p;
    p.then((reply) => sendResponse(reply));
    return true;
  }
});
