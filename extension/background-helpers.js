// Pure connector helpers shared by Firefox's event page, Chromium's service
// worker, and Node's release tests.
(function expose(root, factory) {
  const api = factory();
  root.ldmBackgroundHelpers = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(globalThis, () => {
  function cookieQuery(url, storeId) {
    const query = { url };
    if (storeId !== undefined && storeId !== null && storeId !== "") {
      query.storeId = storeId;
    }
    return query;
  }

  function utf8LengthAtMost(value, maximum) {
    let length = 0;
    for (const character of value) {
      const point = character.codePointAt(0);
      length += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
      if (length > maximum) return length;
    }
    return length;
  }

  function cookieHeader(cookies, maximumBytes = 256 * 1024) {
    let result = "";
    let bytes = 0;
    for (const cookie of cookies || []) {
      const part = `${String(cookie?.name ?? "")}=${String(cookie?.value ?? "")}`;
      const separator = result ? "; " : "";
      const remaining = maximumBytes - bytes - separator.length;
      if (remaining < 0) break;
      const partBytes = utf8LengthAtMost(part, remaining);
      if (partBytes > remaining) break;
      result += separator + part;
      bytes += separator.length + partBytes;
    }
    return result;
  }

  function normalizeDisabledHosts(entries) {
    const hosts = [];
    for (const entry of entries || []) {
      let host = String(entry || "").trim().toLowerCase();
      if (!host) continue;
      try {
        if (host.includes("://")) host = new URL(host).hostname;
      } catch {
        continue;
      }
      host = host.replace(/\.+$/, "");
      if (!host || /[\s/]/.test(host) || host.length > 253) continue;
      if (!hosts.includes(host)) hosts.push(host);
    }
    return hosts;
  }

  function isHostDisabled(url, entries) {
    try {
      const host = new URL(url).hostname.toLowerCase().replace(/\.+$/, "");
      return normalizeDisabledHosts(entries).includes(host);
    } catch {
      return false;
    }
  }

  // Referrer the connector synthesizes from the tab, under the browser's own
  // default policy (strict-origin-when-cross-origin): the full page URL stays
  // on its origin, other hosts see only the origin, and https never tells http.
  function defaultReferrer(pageUrl, targetUrl) {
    try {
      const page = new URL(pageUrl);
      const target = new URL(targetUrl);
      if (!/^https?:$/.test(page.protocol)) return undefined;
      if (page.origin === target.origin) {
        page.username = "";
        page.password = "";
        page.hash = "";
        return page.href;
      }
      if (page.protocol === "https:" && target.protocol !== "https:") return undefined;
      return `${page.origin}/`;
    } catch {
      return undefined;
    }
  }

  async function enrichCaptureJob(job, tab, getCookies) {
    const capture = { ...job };
    const storeId = capture.cookie_store_id || (tab && tab.cookieStoreId);
    if (!capture.cookie && /^https?:/i.test(capture.url || "")) {
      const cookies = await getCookies(cookieQuery(capture.url, storeId));
      const header = cookieHeader(cookies);
      if (header) capture.cookie = header;
    }
    if (!capture.referrer && tab && tab.url) {
      const referrer = defaultReferrer(tab.url, capture.url);
      if (referrer) capture.referrer = referrer;
    }
    if (!capture.cookie_store_id && storeId) capture.cookie_store_id = storeId;
    return capture;
  }

  function decodePercent(value) {
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }

  // Filename from a Content-Disposition header (RFC 6266), else the URL path.
  // The RFC 5987 form (`filename*=UTF-8'lang'value`) wins; a quoted plain
  // `filename` honours backslash escapes and keeps apostrophes.
  function filenameFromCD(cd, url) {
    if (cd) {
      const extended = /filename\*\s*=\s*([^']*)'[^']*'([^;]+)/i.exec(cd);
      if (extended) {
        const value = extended[2].trim().replace(/^"(.*)"$/, "$1");
        if (value) return decodePercent(value);
      }
      const quoted = /(?:^|;)\s*filename\s*=\s*"((?:[^"\\]|\\.)*)"/i.exec(cd);
      if (quoted && quoted[1]) return quoted[1].replace(/\\(.)/g, "$1");
      const token = /(?:^|;)\s*filename\s*=\s*([^;]+)/i.exec(cd);
      if (token && token[1].trim()) return token[1].trim();
    }
    try {
      const name = new URL(url).pathname.split("/").pop();
      if (name) return decodePercent(name);
    } catch {}
    return undefined;
  }

  // Path A (Firefox webRequest) and Path B (downloads.onCreated) can both see
  // one browser download. Each Path A handoff is a record keyed by URL and
  // cookie store (two containers downloading one URL are two captures):
  //   pending -> accepted | rejected     (the app's reply)
  //   rejected -> fallback               (Path B hands the download off itself)
  // Records expire after `ttlMs`; at most `limit` are kept.
  function createHandoffRegistry({ ttlMs = 5000, limit = 512, now = Date.now } = {}) {
    const records = new Map();
    let nextId = 1;
    const keyOf = (url, cookieStoreId) => `${cookieStoreId ?? ""}\n${url}`;
    function prune() {
      const at = now();
      for (const [key, record] of records) {
        if (at - record.at > ttlMs) records.delete(key);
      }
      while (records.size > limit) records.delete(records.keys().next().value);
    }
    function remember(url, cookieStoreId, handoff) {
      const record = { id: nextId++, url, cookieStoreId, state: "pending", at: now(), handoff };
      records.set(keyOf(url, cookieStoreId), record);
      prune();
      record.settled = Promise.resolve(handoff).then(
        (reply) => {
          if (record.state === "pending") record.state = reply && reply.ok ? "accepted" : "rejected";
          return reply;
        },
        () => {
          if (record.state === "pending") record.state = "rejected";
          return null;
        },
      );
      return record;
    }
    function recent(url, cookieStoreId) {
      prune();
      return records.get(keyOf(url, cookieStoreId)) || null;
    }
    // Path B: wait for Path A's decision. "accepted" means drop the browser
    // copy; anything else moves the record to "fallback" and Path B sends it.
    async function decide(url, cookieStoreId) {
      const record = recent(url, cookieStoreId);
      if (!record) return "none";
      await record.settled;
      if (record.state === "accepted") return "accepted";
      record.state = "fallback";
      return "fallback";
    }
    return { remember, recent, decide };
  }

  // Lowercased trailing extension of a filename or URL path ("" when none).
  function extOf(name) {
    const m = /\.([a-z0-9]{1,8})$/i.exec((name || "").split(/[?#]/)[0]);
    return m ? m[1].toLowerCase() : "";
  }

  // True when the filename or any of `urls` (the clicked URL and where it
  // redirected) has a file type on the user's blacklist.
  function isBlacklisted(urls, filename, list) {
    if (!list || !list.length) return false;
    const types = [extOf(filename)];
    for (const url of urls) {
      try {
        types.push(extOf(new URL(url).pathname));
      } catch {}
    }
    return types.some((type) => type && list.includes(type));
  }

  function addBoundedMedia(list, entry, limit = 100) {
    if (!entry || !/^https?:/i.test(entry.url || "") || entry.url.length > 8192) {
      return list.slice(0, limit);
    }
    if (list.some((item) => item.url === entry.url)) return list.slice(0, limit);
    return [...list, entry].slice(-limit);
  }

  return {
    addBoundedMedia,
    cookieHeader,
    createHandoffRegistry,
    filenameFromCD,
    isBlacklisted,
    cookieQuery,
    defaultReferrer,
    enrichCaptureJob,
    isHostDisabled,
    normalizeDisabledHosts,
  };
});
