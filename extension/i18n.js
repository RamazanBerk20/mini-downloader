// Localize static HTML pages. WebExtensions do not auto-translate HTML text, so
// on DOMContentLoaded we swap in messages from the i18n API for every element
// carrying a data-i18n* attribute. English text stays inline as the fallback.
//
// A manual language override (settings.lang) loads that locale's packaged
// messages.json and takes precedence over the browser's UI language — the
// browser.i18n API itself cannot switch locales at runtime.
(function () {
  const b = globalThis.browser || globalThis.chrome;
  const browserMsg = (k) => (b && b.i18n && k ? b.i18n.getMessage(k) : "");
  let override = null; // { code, table } when a manual language is set

  const msg = (k) => {
    if (override) {
      const e = override.table[k];
      if (e && e.message) return e.message;
    }
    return browserMsg(k);
  };

  function applyDir() {
    const dir = override ? (override.code === "ar" ? "rtl" : "ltr") : browserMsg("@@bidi_dir");
    if (dir) document.documentElement.dir = dir;
  }

  function apply() {
    for (const el of document.querySelectorAll("[data-i18n]")) {
      const t = msg(el.getAttribute("data-i18n"));
      if (t) el.textContent = t;
    }
    for (const el of document.querySelectorAll("[data-i18n-placeholder]")) {
      const t = msg(el.getAttribute("data-i18n-placeholder"));
      if (t) el.setAttribute("placeholder", t);
    }
    for (const el of document.querySelectorAll("[data-i18n-title]")) {
      const t = msg(el.getAttribute("data-i18n-title"));
      if (t) el.setAttribute("title", t);
    }
  }

  const ready = (async () => {
    try {
      const s = (await b.storage.local.get("settings")).settings || {};
      const code = s.lang || "";
      if (code && /^[a-z]{2}$/.test(code)) {
        const res = await fetch(b.runtime.getURL(`_locales/${code}/messages.json`));
        override = { code, table: await res.json() };
      }
    } catch {
      override = null;
    }
    applyDir();
    if (document.readyState === "loading") {
      await new Promise((r) => document.addEventListener("DOMContentLoaded", r, { once: true }));
    }
    apply();
  })();

  // Pages read translations through this (options.js / popup.js): await `ready`
  // before rendering dynamic strings so the override table is loaded.
  globalThis.ldmI18n = { t: msg, ready };
})();
