const b = globalThis.browser || globalThis.chrome;
const t = (k) => globalThis.ldmI18n.t(k);
const { SIZE_UNITS, displaySize, preferredSizeUnit, sizeToBytes, validSizeUnit } =
  globalThis.ldmOptionsHelpers;
// Keep the manual bridge test on the same browser-family wire contract as the
// background heartbeat and captured jobs. This matters for supported forks
// whose native-host launch arguments do not identify the calling browser.
const BROWSER_FAMILY = typeof globalThis.browser !== "undefined" ? "firefox" : "chromium";
const DEFAULTS = {
  enabled: true,
  minSize: 1048576,
  minSizeUnit: "MB",
  disabledHosts: [],
  blacklistExts: [],
  lang: "",
};

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

function sizeInBytes() {
  const input = document.getElementById("minSize");
  const unit = document.getElementById("minSizeUnit").value;
  const value = input.valueAsNumber;
  return sizeToBytes(value, unit);
}

function updateSizeStep() {
  const unit = document.getElementById("minSizeUnit").value;
  document.getElementById("minSize").step = unit === "B" ? "1" : "0.1";
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
  const bytes = Math.max(0, Number(cfg.minSize) || 0);
  const sizeUnit = validSizeUnit(cfg.minSizeUnit) ? cfg.minSizeUnit : preferredSizeUnit(bytes);
  document.getElementById("minSizeUnit").value = sizeUnit;
  document.getElementById("minSizeUnit").dataset.previous = sizeUnit;
  document.getElementById("minSize").value = displaySize(bytes, sizeUnit);
  updateSizeStep();
  document.getElementById("disabledHosts").value = (cfg.disabledHosts || []).join("\n");
  document.getElementById("blacklistExts").value = (cfg.blacklistExts || []).join(", ");
}

async function save() {
  const minSize = sizeInBytes();
  if (minSize === null) {
    const input = document.getElementById("minSize");
    input.focus();
    input.reportValidity();
    return;
  }
  const lang = document.getElementById("lang").value;
  const cfg = {
    enabled: document.getElementById("enabled").checked,
    minSize,
    minSizeUnit: document.getElementById("minSizeUnit").value,
    disabledHosts: globalThis.ldmBackgroundHelpers.normalizeDisabledHosts(
      document.getElementById("disabledHosts").value.split("\n"),
    ),
    // Normalized: lowercase, no leading dot — matches background.js lookups.
    blacklistExts: document
      .getElementById("blacklistExts")
      .value.split(",")
      .map((s) => s.trim().toLowerCase().replace(/^\./, ""))
      .filter(Boolean),
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
document.getElementById("minSizeUnit").addEventListener("change", (event) => {
  const input = document.getElementById("minSize");
  const previousUnit = event.target.dataset.previous || DEFAULTS.minSizeUnit;
  const value = input.valueAsNumber;
  const bytes = Number.isFinite(value) && value >= 0 ? value * SIZE_UNITS[previousUnit] : 0;
  input.value = displaySize(bytes, event.target.value);
  event.target.dataset.previous = event.target.value;
  updateSizeStep();
});

// Native-host health check: confirm the bridge reaches the running app.
document.getElementById("test").addEventListener("click", async () => {
  const st = document.getElementById("test-status");
  setStatus(st, "…", "busy");
  try {
    const reply = await b.runtime.sendNativeMessage("com.minidownloader.host", {
      ping: true,
      browserFamily: BROWSER_FAMILY,
    });
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
