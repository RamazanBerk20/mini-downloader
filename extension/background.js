// LDM Connector — background/event page.
// Path A (primary): webRequest header sniff + cancel → full cookie/header fidelity.
// Path B (fallback): downloads.onCreated + cancel/erase + cookies.getAll.
// Both forward a CaptureJob to the native host over native messaging.

const b = globalThis.browser || globalThis.chrome;
const HOST = "com.ldm.host";

const DEFAULTS = {
  enabled: true,
  minSize: 1048576, // 1 MiB floor: ignore tiny/inline files
  disabledHosts: [],
};
let settings = { ...DEFAULTS };

async function loadSettings() {
  try {
    const s = await b.storage.local.get("settings");
    settings = { ...DEFAULTS, ...(s.settings || {}) };
  } catch {}
}
loadSettings();
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
    const reply = await b.runtime.sendNativeMessage(HOST, job);
    if (reply && reply.ok) notify("Sent to LDM", job.filename || job.url);
    else notify("LDM rejected the download", (reply && reply.error) || "unknown error");
    return reply;
  } catch (e) {
    notify("LDM not reachable", "Open Linux Download Manager and enable browser integration.");
    return null;
  }
}

// Dedup the same URL across Path A / Path B within a short window.
const recent = new Map();
function seen(url) {
  const now = Date.now();
  for (const [k, t] of recent) if (now - t > 5000) recent.delete(k);
  if (recent.has(url)) return true;
  recent.set(url, now);
  return false;
}

// ---------- Path A: webRequest ----------

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
  (d) => {
    if (!enabledForUrl(d.url)) return {};
    const h = {};
    for (const x of d.responseHeaders || []) h[x.name.toLowerCase()] = x.value;
    const cd = h["content-disposition"] || "";
    const ct = h["content-type"] || "";
    const lenRaw = parseInt(h["content-length"] || "-1", 10);
    const len = Number.isNaN(lenRaw) ? -1 : lenRaw;
    if (!shouldHijack(cd, ct, len, d.url)) return {};
    if (seen(d.url)) return { cancel: true };

    const job = jobFromRequest(
      d.url,
      filenameFromCD(cd, d.url),
      (ct.split(";")[0] || "").trim(),
      len,
      reqHeaders.get(d.requestId) || [],
      d.cookieStoreId,
    );
    sendJob(job);
    return { cancel: true };
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] },
  ["blocking", "responseHeaders"],
);

// ---------- Path B: downloads.onCreated fallback ----------

b.downloads.onCreated.addListener(async (item) => {
  try {
    if (item.byExtensionId === b.runtime.id) return;
    if (/^blob:|^data:/i.test(item.url)) return;
    if (!enabledForUrl(item.url)) return;
    if (item.totalBytes > -1 && item.totalBytes < settings.minSize) return;
    if (seen(item.url)) return;

    await b.downloads.cancel(item.id).catch(() => {});
    await b.downloads.erase({ id: item.id }).catch(() => {});

    let cookie = "";
    try {
      const cs = await b.cookies.getAll({ url: item.url, storeId: item.cookieStoreId });
      cookie = cs.map((c) => `${c.name}=${c.value}`).join("; ");
    } catch {}

    sendJob({
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
  } catch {}
});

// ---------- Context menu ----------

function createMenu() {
  try {
    b.contextMenus.removeAll(() => {
      b.contextMenus.create({
        id: "ldm-download",
        title: "Download with LDM",
        contexts: ["link", "video", "audio", "image", "selection"],
      });
    });
  } catch {}
}
b.runtime.onInstalled.addListener(createMenu);
b.runtime.onStartup.addListener(createMenu);
createMenu();

b.contextMenus.onClicked.addListener(async (info, tab) => {
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

const mediaByTab = new Map();
b.webRequest.onResponseStarted.addListener(
  (d) => {
    const isHls = /\.m3u8(\?|$)|mpegurl/i.test(d.url);
    const isDash = /\.mpd(\?|$)|dash\+xml/i.test(d.url);
    if (!isHls && !isDash) return;
    const list = mediaByTab.get(d.tabId) || [];
    if (!list.some((m) => m.url === d.url)) {
      list.push({ url: d.url, type: isDash ? "dash" : "hls" });
      mediaByTab.set(d.tabId, list);
    }
  },
  { urls: ["<all_urls>"], types: ["xmlhttprequest", "media", "other"] },
);
b.tabs.onRemoved.addListener((id) => mediaByTab.delete(id));
b.tabs.onUpdated.addListener((id, ch) => {
  if (ch.url) mediaByTab.delete(id);
});

b.runtime.onMessage.addListener((msg, sender) => {
  if (!msg) return;
  if (msg.type === "ldm-media" && sender.tab) {
    const list = mediaByTab.get(sender.tab.id) || [];
    for (const u of msg.urls || []) {
      if (!list.some((m) => m.url === u)) list.push({ url: u, type: "file" });
    }
    mediaByTab.set(sender.tab.id, list);
    return;
  }
  if (msg.type === "ldm-get-media") {
    return Promise.resolve(mediaByTab.get(msg.tabId) || []);
  }
  if (msg.type === "ldm-grab") {
    return sendJob(msg.job);
  }
});
