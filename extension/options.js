const b = globalThis.browser || globalThis.chrome;
const t = (k) => globalThis.ldmI18n.t(k);
const DEFAULTS = {
  enabled: true,
  minSize: 1048576,
  disabledHosts: [],
  blacklistExts: [],
  blacklistMagnet: false,
  lang: "",
};
const MIB = 1048576;

// Manual language override choices; "" follows the browser's UI language.
// Native names, deliberately not translated.
const LANGS = [
  ["en", "English"],
  ["tr", "Türkçe"],
  ["es", "Español"],
  ["fr", "Français"],
  ["de", "Deutsch"],
  ["ru", "Русский"],
  ["ar", "العربية"],
  ["zh", "中文"],
  ["ja", "日本語"],
  ["ko", "한국어"],
];

// Set text + visual state (ok/err/busy) on a status span in one go.
function setStatus(el, text, state) {
  el.textContent = text;
  el.className = "status" + (state ? " status-" + state : "");
}

// Live hint next to the raw-bytes input; storage stays bytes.
function updateMibHint() {
  const bytes = parseInt(document.getElementById("minSize").value, 10) || 0;
  const mib = Math.round((bytes / MIB) * 100) / 100;
  document.getElementById("minSizeMiB").textContent = "= " + mib + " MiB";
}

let savedLang = "";

async function load() {
  await globalThis.ldmI18n.ready;
  const s = (await b.storage.local.get("settings")).settings || {};
  const cfg = { ...DEFAULTS, ...s };
  const sel = document.getElementById("lang");
  const def = document.createElement("option");
  def.value = "";
  def.textContent = t("optionsLangBrowser");
  sel.appendChild(def);
  for (const [code, name] of LANGS) {
    const o = document.createElement("option");
    o.value = code;
    o.textContent = name;
    sel.appendChild(o);
  }
  sel.value = cfg.lang || "";
  savedLang = cfg.lang || "";
  document.getElementById("enabled").checked = cfg.enabled;
  document.getElementById("minSize").value = cfg.minSize;
  document.getElementById("disabledHosts").value = (cfg.disabledHosts || []).join("\n");
  document.getElementById("blacklistExts").value = (cfg.blacklistExts || []).join(", ");
  document.getElementById("blacklistMagnet").checked = !!cfg.blacklistMagnet;
  updateMibHint();
}

async function save() {
  const lang = document.getElementById("lang").value;
  const cfg = {
    enabled: document.getElementById("enabled").checked,
    minSize: parseInt(document.getElementById("minSize").value, 10) || 0,
    disabledHosts: document
      .getElementById("disabledHosts")
      .value.split("\n")
      .map((s) => s.trim())
      .filter(Boolean),
    // Normalized: lowercase, no leading dot — matches background.js lookups.
    blacklistExts: document
      .getElementById("blacklistExts")
      .value.split(",")
      .map((s) => s.trim().toLowerCase().replace(/^\./, ""))
      .filter(Boolean),
    blacklistMagnet: document.getElementById("blacklistMagnet").checked,
    lang,
  };
  await b.storage.local.set({ settings: cfg });
  if (lang !== savedLang) {
    // Re-run i18n with the new override.
    location.reload();
    return;
  }
  const st = document.getElementById("status");
  setStatus(st, t("optionsSaved"), "ok");
  setTimeout(() => setStatus(st, "", null), 1500);
}

document.getElementById("save").addEventListener("click", save);
document.getElementById("minSize").addEventListener("input", updateMibHint);

// Native-host health check: confirm the bridge reaches the running app.
document.getElementById("test").addEventListener("click", async () => {
  const st = document.getElementById("test-status");
  setStatus(st, "…", "busy");
  try {
    const reply = await b.runtime.sendNativeMessage("com.minidownloader.host", { ping: true });
    if (reply && reply.ok) {
      setStatus(st, reply.error || t("optionsTestOk"), "ok");
    } else {
      setStatus(st, t("optionsTestNoResponse"), "err");
    }
  } catch (e) {
    setStatus(st, t("optionsTestFail"), "err");
  }
});

load();
