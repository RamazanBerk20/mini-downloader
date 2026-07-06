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

  // On-demand harvest for the "download all links / images" context menu.
  const FILE_RE =
    /\.(zip|7z|rar|tar|gz|tgz|xz|bz2|zst|iso|img|dmg|exe|msi|deb|rpm|appimage|pkg|apk|mp4|mkv|webm|avi|mov|flv|mp3|flac|m4a|wav|ogg|opus|pdf|epub|bin|run)(\?|$)/i;
  b.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || msg.type !== "ldm-harvest") return;
    const urls = new Set();
    if (msg.what === "images") {
      for (const img of document.images) {
        if (img.src && !/^data:/i.test(img.src)) urls.add(img.src);
      }
    } else {
      for (const a of document.querySelectorAll("a[href]")) {
        try {
          const u = new URL(a.href);
          if (/^https?:$/.test(u.protocol) && FILE_RE.test(u.pathname)) urls.add(u.href);
        } catch {}
      }
    }
    sendResponse([...urls]);
    return true;
  });

  collect();
  // Re-scan on DOM changes (players inject media late).
  let timer = null;
  const obs = new MutationObserver(() => {
    clearTimeout(timer);
    timer = setTimeout(collect, 800);
  });
  obs.observe(document.documentElement, { childList: true, subtree: true });
})();
