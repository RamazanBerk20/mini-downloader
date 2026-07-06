const b = globalThis.browser || globalThis.chrome;

async function currentTab() {
  const [tab] = await b.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function init() {
  const s = await b.storage.local.get("settings");
  const settings = s.settings || { enabled: true };
  document.getElementById("enabled").checked = settings.enabled !== false;

  document.getElementById("enabled").addEventListener("change", async (e) => {
    const cur = (await b.storage.local.get("settings")).settings || {};
    cur.enabled = e.target.checked;
    await b.storage.local.set({ settings: cur });
  });

  document.getElementById("opts").addEventListener("click", (e) => {
    e.preventDefault();
    b.runtime.openOptionsPage();
  });

  const tab = await currentTab();

  document.getElementById("grabpage").addEventListener("click", () => {
    if (!tab) return;
    // Hand the page URL to yt-dlp (kind hls is a hint that yt-dlp should drive).
    b.runtime.sendMessage({
      type: "ldm-grab",
      job: { url: tab.url, page_url: tab.url, kind: "hls", extra_headers: [] },
    });
  });

  const media = await b.runtime.sendMessage({ type: "ldm-get-media", tabId: tab.id });
  render(media || [], tab);
}

function render(list, tab) {
  const root = document.getElementById("media");
  if (!list.length) {
    root.innerHTML = '<p class="empty">No media detected.</p>';
    return;
  }
  root.innerHTML = "";
  for (const m of list) {
    const div = document.createElement("div");
    div.className = "item";
    const short = m.url.length > 70 ? m.url.slice(0, 70) + "…" : m.url;
    div.innerHTML = `<div class="tag">${m.type}</div><div class="u">${short}</div>`;
    const btn = document.createElement("button");
    btn.textContent = "Grab";
    btn.addEventListener("click", () => {
      const kind = m.type === "dash" ? "dash" : m.type === "hls" ? "hls" : "http";
      b.runtime.sendMessage({
        type: "ldm-grab",
        job: { url: m.url, page_url: tab && tab.url, kind, extra_headers: [] },
      });
      btn.textContent = "Sent";
      btn.disabled = true;
    });
    div.appendChild(btn);
    root.appendChild(div);
  }
}

init();
