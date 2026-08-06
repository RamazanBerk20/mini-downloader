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

  async function enrichCaptureJob(job, tab, getCookies) {
    const capture = { ...job };
    const storeId = capture.cookie_store_id || (tab && tab.cookieStoreId);
    if (!capture.cookie && /^https?:/i.test(capture.url || "")) {
      const cookies = await getCookies(cookieQuery(capture.url, storeId));
      const header = cookieHeader(cookies);
      if (header) capture.cookie = header;
    }
    if (!capture.referrer && tab && tab.url) capture.referrer = tab.url;
    if (!capture.cookie_store_id && storeId) capture.cookie_store_id = storeId;
    return capture;
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
    cookieQuery,
    enrichCaptureJob,
    isHostDisabled,
    normalizeDisabledHosts,
  };
});
