// Localize static HTML pages. WebExtensions do not auto-translate HTML text, so
// on DOMContentLoaded we swap in messages from the i18n API for every element
// carrying a data-i18n* attribute. English text stays inline as the fallback.
(function () {
  const b = globalThis.browser || globalThis.chrome;
  const msg = (k) => (b && b.i18n && k ? b.i18n.getMessage(k) : "");

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

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", apply);
  } else {
    apply();
  }
})();
