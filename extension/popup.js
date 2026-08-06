const b = globalThis.browser || globalThis.chrome;
const t = (k) => globalThis.ldmI18n.t(k);

async function currentTab() {
  const [tab] = await b.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function submitGrab(button, job, isPrivate = false) {
  button.disabled = true;
  button.removeAttribute("title");
  button.classList.remove("sent", "failed");
  button.textContent = t("popupSending");
  try {
    const reply = await b.runtime.sendMessage({ type: "ldm-grab", job, private: isPrivate });
    if (reply && reply.ok) {
      button.textContent = t("popupSent");
      button.classList.add("sent");
      return;
    }
    button.textContent = t("popupFailed");
    button.classList.add("failed");
  } catch {
    button.textContent = t("popupFailed");
    button.classList.add("failed");
  }
  button.disabled = false;
  button.title = t("popupRetryHint");
}

async function init() {
  await globalThis.ldmI18n.ready;
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

  document.getElementById("grabpage").addEventListener("click", async (event) => {
    if (!tab) return;
    // Hand the page URL to yt-dlp (kind hls is a hint that yt-dlp should drive).
    await submitGrab(
      event.currentTarget,
      { url: tab.url, page_url: tab.url, kind: "hls", extra_headers: [] },
      Boolean(tab.incognito),
    );
  });

  // `tab` can be undefined if no active tab is queryable — guard the deref.
  const media = tab
    ? await b.runtime.sendMessage({
        type: "ldm-get-media",
        tabId: tab.id,
        private: Boolean(tab.incognito),
      })
    : [];
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
    div.className = "card";
    const short = m.url.length > 70 ? m.url.slice(0, 70) + "…" : m.url;
    // URLs are page-controlled: build with textContent, never innerHTML.
    // (title is set via attribute assignment, which is equally inert.)
    const badge = m.type === "dash" ? "dash" : m.type === "hls" ? "hls" : "file";
    const tag = document.createElement("span");
    tag.className = "tag " + badge;
    tag.textContent = m.type;
    const u = document.createElement("div");
    u.className = "u";
    u.textContent = short;
    u.title = m.url;
    const btn = document.createElement("button");
    btn.className = "btn";
    btn.textContent = t("popupGrab");
    btn.addEventListener("click", async () => {
      const kind = m.type === "dash" ? "dash" : m.type === "hls" ? "hls" : "http";
      await submitGrab(
        btn,
        { url: m.url, page_url: tab && tab.url, kind, extra_headers: [] },
        Boolean(tab && tab.incognito),
      );
    });
    div.append(tag, u, btn);
    root.appendChild(div);
  }
}

init();
