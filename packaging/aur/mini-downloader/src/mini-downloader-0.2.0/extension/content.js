// Scan the page for direct media sources and report them to the background.
// blob:/MediaSource sources aren't directly fetchable, so those are skipped —
// the popup falls back to the page URL via yt-dlp for those.

(function () {
  const b = globalThis.browser || globalThis.chrome;

  function collect() {
    const urls = new Set();
    for (const el of document.querySelectorAll("video, audio")) {
      const src = el.currentSrc || el.src;
      if (src && !/^blob:|^data:/i.test(src)) urls.add(src);
    }
    for (const s of document.querySelectorAll("source[src]")) {
      const src = s.src;
      if (src && !/^blob:|^data:/i.test(src)) urls.add(src);
    }
    if (urls.size) {
      b.runtime.sendMessage({ type: "ldm-media", urls: [...urls] }).catch(() => {});
    }
  }

  collect();
  // Re-scan on DOM changes (players inject media late).
  let timer = null;
  const obs = new MutationObserver(() => {
    clearTimeout(timer);
    timer = setTimeout(collect, 800);
  });
  obs.observe(document.documentElement, { childList: true, subtree: true });
})();
