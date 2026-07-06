const b = globalThis.browser || globalThis.chrome;
const t = (k) => b.i18n.getMessage(k);
const DEFAULTS = { enabled: true, minSize: 1048576, disabledHosts: [] };

async function load() {
  const s = (await b.storage.local.get("settings")).settings || {};
  const cfg = { ...DEFAULTS, ...s };
  document.getElementById("enabled").checked = cfg.enabled;
  document.getElementById("minSize").value = cfg.minSize;
  document.getElementById("disabledHosts").value = (cfg.disabledHosts || []).join("\n");
}

async function save() {
  const cfg = {
    enabled: document.getElementById("enabled").checked,
    minSize: parseInt(document.getElementById("minSize").value, 10) || 0,
    disabledHosts: document
      .getElementById("disabledHosts")
      .value.split("\n")
      .map((s) => s.trim())
      .filter(Boolean),
  };
  await b.storage.local.set({ settings: cfg });
  const st = document.getElementById("status");
  st.textContent = t("optionsSaved");
  setTimeout(() => (st.textContent = ""), 1500);
}

document.getElementById("save").addEventListener("click", save);
load();
