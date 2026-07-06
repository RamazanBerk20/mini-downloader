const b = globalThis.browser || globalThis.chrome;
const t = (k) => b.i18n.getMessage(k);

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
  root.textContent = "";
  if (!list.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent = t("popupNoMedia");
    root.appendChild(p);
    return;
  }
  for (const m of list) {
    const div = document.createElement("div");
    div.className = "item";
    const short = m.url.length > 70 ? m.url.slice(0, 70) + "…" : m.url;
    // URLs are page-controlled: build with textContent, never innerHTML.
    const tag = document.createElement("div");
    tag.className = "tag";
    tag.textContent = m.type;
    const u = document.createElement("div");
    u.className = "u";
    u.textContent = short;
    div.append(tag, u);
    const btn = document.createElement("button");
    btn.textContent = t("popupGrab");
    btn.addEventListener("click", () => {
      const kind = m.type === "dash" ? "dash" : m.type === "hls" ? "hls" : "http";
      b.runtime.sendMessage({
        type: "ldm-grab",
        job: { url: m.url, page_url: tab && tab.url, kind, extra_headers: [] },
      });
      btn.textContent = t("popupSent");
      btn.disabled = true;
    });
    div.appendChild(btn);
    root.appendChild(div);
  }
}

init();
