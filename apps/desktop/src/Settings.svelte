<script lang="ts">
  import { onMount, tick } from "svelte";
  import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
  import { open } from "@tauri-apps/plugin-dialog";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api, errText, type ConnectorStatus } from "./api";

  const SPONSOR_URL = "https://github.com/sponsors/RamazanBerk20";
  import { trapFocus } from "./lib/a11y";
  import Icon from "./lib/Icon.svelte";
  import type { Category, Schedule, UpdateInfo } from "./types";
  import { t, LOCALES, setLocale, normalizeLocale, type LocaleCode, type MsgKey } from "./lib/i18n.svelte";

  // Locale-neutral public store listings. Browser extensions must be installed
  // with the browser's own consent UI; see scripts/EXTENSION-PUBLISHING.md.
  const STORE_URLS = {
    firefox: "https://addons.mozilla.org/firefox/addon/mini-downloader-connector/",
    chrome: "https://chromewebstore.google.com/detail/mini-downloader-connector/hhaobmkdgijodfieadeeanjmnneckafj",
  };

  let {
    onclose,
    initialSection = null,
    connectorStatus = null,
  }: {
    onclose: () => void;
    initialSection?: "extensions" | null;
    connectorStatus?: ConnectorStatus | null;
  } = $props();

  type SettingsSection =
    | "general"
    | "network"
    | "language"
    | "connections"
    | "extensions"
    | "scheduler"
    | "categories"
    | "updates";
  type BrowserFamily = "firefox" | "chromium";
  type BrowserState =
    | "browserStateChecking"
    | "browserStateConnected"
    | "browserStateInstalled"
    | "browserStateNeedsExtension"
    | "browserStateNotFound";

  const SECTION_LINKS: Array<[SettingsSection, MsgKey]> = [
    ["general", "sectGeneral"],
    ["network", "sectNetwork"],
    ["language", "sectLanguage"],
    ["connections", "sectConnections"],
    ["extensions", "sectExtensions"],
    ["scheduler", "sectScheduler"],
    ["categories", "sectCategories"],
    ["updates", "sectUpdates"],
  ];

  let autoOrganize = $state(true);
  let clipboardWatch = $state(false);
  let closeToTray = $state(true);
  let autostart = $state(false);
  let categories = $state<Category[]>([]);
  let schedules = $state<Schedule[]>([]);
  let settingsStatus = $state("");
  let extensionsSection = $state<HTMLElement | null>(null);
  let drawer = $state<HTMLElement | null>(null);
  let activeSection = $state<SettingsSection>("general");
  let split = $state(16);
  let connections = $state(16);

  // A profile is the useful definition of an available browser here: it is
  // where the connector is installed and where the native host is registered.
  // With no known profile, retain both choices for a newly installed browser.
  const showFirefoxStore = $derived(
    connectorStatus === null ||
      connectorStatus.firefoxProfileDetected ||
      !connectorStatus.chromiumProfileDetected,
  );
  const showChromiumStore = $derived(
    connectorStatus === null ||
      connectorStatus.chromiumProfileDetected ||
      !connectorStatus.firefoxProfileDetected,
  );

  function browserState(family: BrowserFamily): BrowserState {
    if (!connectorStatus) return "browserStateChecking";
    const connected = family === "firefox" ? connectorStatus.firefoxDetected : connectorStatus.chromiumDetected;
    const installed = family === "firefox" ? connectorStatus.firefoxConnectorInstalled : connectorStatus.chromiumConnectorInstalled;
    const profile = family === "firefox" ? connectorStatus.firefoxProfileDetected : connectorStatus.chromiumProfileDetected;
    if (connected) return "browserStateConnected";
    if (installed) return "browserStateInstalled";
    if (profile) return "browserStateNeedsExtension";
    return "browserStateNotFound";
  }
  const firefoxState = $derived(browserState("firefox"));
  const chromiumState = $derived(browserState("chromium"));

  const DAYS = ["dayMon", "dayTue", "dayWed", "dayThu", "dayFri", "daySat", "daySun"] as const;
  const ACTIONS: [string, MsgKey][] = [
    ["pause_all", "schedPauseAll"],
    ["resume_all", "schedResumeAll"],
    ["set_speed", "schedSetSpeed"],
  ];

  let nAction = $state("pause_all");
  let nTime = $state("22:00");
  let nDays = $state<Set<number>>(new Set([0, 1, 2, 3, 4, 5, 6]));
  let nSpeed = $state("512000");
  let schedError = $state("");
  let theme = $state("system");
  let onComplete = $state("none");
  let maxConcurrent = $state(5);
  let ncName = $state("");
  let ncDir = $state("");
  let ncRules = $state("");
  let ncMime = $state("");
  let ncHost = $state("");
  let proxy = $state("");
  let handleMagnets = $state(true);
  let dhtEnabled = $state(true);
  let sandboxChildren = $state(false);
  let blockPrivateIps = $state(false);
  let ocCustomCmd = $state("");
  let ocCustomConfirmed = $state(false);
  let customCommandRevision = 0;
  // Persist consent updates in input order. Without this queue, a slow write
  // of `false` from an edited command could land after a newer confirmation
  // and silently disable (or, worse, preserve) the wrong command state.
  let customConfirmationWrite: Promise<void> = Promise.resolve();
  let updateStatus = $state("");
  let update = $state<UpdateInfo | null>(null);
  let langChoice = $state("system");
  let defaultSpeedKb = $state(0);

  const OC_ACTIONS: [string, MsgKey][] = [
    ["none", "ocNone"],
    ["quit", "ocQuit"],
    ["sleep", "ocSleep"],
    ["shutdown", "ocShutdown"],
    ["custom", "ocCustom"],
  ];

  onMount(async () => {
    autoOrganize = (await api.getSetting("auto_organize")) !== "false";
    clipboardWatch = (await api.getSetting("clipboard_watch")) === "true";
    closeToTray = (await api.getSetting("close_to_tray")) !== "false";
    theme = (await api.getSetting("theme")) || "system";
    onComplete = (await api.getSetting("on_complete_action")) || "none";
    // A stored `run:<cmd>` value means the custom action is selected.
    if (onComplete.startsWith("run:")) {
      ocCustomCmd = onComplete.slice(4);
      onComplete = "custom";
      ocCustomConfirmed = (await api.getSetting("on_complete_command_confirmed")) === "true";
    }
    proxy = (await api.getSetting("proxy")) || "";
    handleMagnets = (await api.getSetting("handle_magnets")) !== "false";
    dhtEnabled = (await api.getSetting("dht_enabled")) !== "false";
    sandboxChildren = (await api.getSetting("sandbox_children")) === "true";
    blockPrivateIps = (await api.getSetting("block_private_ips")) === "true";
    const savedLoc = await api.getSetting("locale");
    langChoice = savedLoc && savedLoc !== "system" ? savedLoc : "system";
    const dsl = await api.getSetting("default_speed_limit");
    defaultSpeedKb = dsl ? Math.round(parseInt(dsl, 10) / 1024) : 0;
    categories = await api.listCategories();
    schedules = await api.listSchedules();
    try {
      autostart = await isEnabled();
    } catch {}
    try {
      [split, connections] = await api.getEngineDefaults();
    } catch {}
    try {
      maxConcurrent = await api.getMaxConcurrent();
    } catch {}
  });

  async function focusExtensions() {
    await tick();
    if (initialSection !== "extensions" || !extensionsSection) return;
    goToSection("extensions", true);
  }

  $effect(() => {
    if (initialSection === "extensions" && extensionsSection) void focusExtensions();
  });

  function applyTheme(v: string) {
    const root = document.documentElement;
    if (v === "light" || v === "dark") root.dataset.theme = v;
    else delete root.dataset.theme;
    // Mirror for the synchronous first-paint apply in main.ts.
    try {
      localStorage.setItem("theme", v);
    } catch {}
  }
  async function onThemeChange(e: Event) {
    theme = (e.target as HTMLSelectElement).value;
    applyTheme(theme);
    await api.setSetting("theme", theme);
  }
  async function onCompleteChange(e: Event) {
    onComplete = (e.target as HTMLSelectElement).value;
    // Selecting a command never enables it by itself. The explicit check below
    // is required after each command change.
    if (onComplete === "custom") {
      await invalidateCustomCommand();
      return;
    }
    await saveOnComplete();
  }
  async function saveOnComplete() {
    const revision = customCommandRevision;
    const command = ocCustomCmd.trim();
    if (onComplete === "custom") {
      if (!command) {
        settingsStatus = t("ocCustomRequired");
        return;
      }
      if (!ocCustomConfirmed) {
        settingsStatus = t("ocCustomConfirmRequired");
        return;
      }
    }
    try {
      const value = onComplete === "custom" ? `run:${command}` : onComplete;
      await api.setSetting("on_complete_action", value);
      // If the command changed while its action value was being saved, leave
      // it explicitly disabled. The queued write preserves event ordering.
      const enableCustomCommand =
        onComplete === "custom" && ocCustomConfirmed && revision === customCommandRevision;
      await persistCustomConfirmation(enableCustomCommand);
      settingsStatus = "";
    } catch (e) {
      settingsStatus = t("settingsSaveError", { msg: errText(e) });
    }
  }
  let proxyStatus = $state("");
  async function saveProxy() {
    try {
      await api.applyProxy(proxy.trim());
      proxyStatus = "";
    } catch (e) {
      proxyStatus = errText(e);
    }
  }
  async function onMaxConcurrent(e: Event) {
    maxConcurrent = parseInt((e.target as HTMLInputElement).value, 10);
    await api.setMaxConcurrent(maxConcurrent);
  }
  async function addCategory() {
    if (!ncName.trim() || !ncDir.trim()) return;
    const split = (s: string) => s.split(",").map((x) => x.trim()).filter(Boolean);
    const exts = split(ncRules);
    const mimes = split(ncMime);
    const hosts = split(ncHost);
    // Ext-only categories keep the legacy flat-array shape; mime/host rules use
    // the object list the classifier also understands.
    const rules =
      mimes.length || hosts.length
        ? JSON.stringify(
            [
              exts.length ? { match: "ext", values: exts } : null,
              mimes.length ? { match: "mime", values: mimes } : null,
              hosts.length ? { match: "host", values: hosts } : null,
            ].filter(Boolean),
          )
        : JSON.stringify(exts);
    await api.saveCategory(ncName.trim(), ncDir.trim(), rules, 0);
    ncName = "";
    ncDir = "";
    ncRules = "";
    ncMime = "";
    ncHost = "";
    categories = await api.listCategories();
  }
  async function savePriority(c: Category, e: Event) {
    const p = parseInt((e.target as HTMLInputElement).value, 10);
    if (Number.isNaN(p)) return;
    await api.saveCategory(c.name, c.dir, c.rules, p);
    categories = await api.listCategories();
  }
  async function removeCategory(id: number) {
    await api.deleteCategory(id);
    categories = await api.listCategories();
  }
  async function browseNewDir() {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") ncDir = dir;
  }
  async function checkForUpdates() {
    updateStatus = t("updateChecking");
    update = null;
    try {
      const u = await api.checkUpdate();
      update = u;
      updateStatus = u.newer ? t("updateAvailable", { v: u.latest }) : t("updateUpToDate", { v: u.current });
    } catch (e) {
      updateStatus = errText(e);
    }
  }
  async function installUpdate() {
    if (!update) return;
    try {
      await api.installUpdate(update.asset_url, update.url);
    } catch (e) {
      updateStatus = errText(e);
    }
  }

  async function saveEngine() {
    try {
      await api.setEngineDefaults(split, connections);
    } catch (e) {
      settingsStatus = t("settingsEngineError", { msg: errText(e) });
    }
  }

  const setBool = (key: string, v: boolean) => api.setSetting(key, v ? "true" : "false");

  async function toggleOrganize(e: Event) {
    autoOrganize = (e.target as HTMLInputElement).checked;
    await setBool("auto_organize", autoOrganize);
  }
  async function toggleClipboard(e: Event) {
    clipboardWatch = (e.target as HTMLInputElement).checked;
    await api.setClipboardWatch(clipboardWatch);
  }
  async function toggleCloseToTray(e: Event) {
    closeToTray = (e.target as HTMLInputElement).checked;
    await setBool("close_to_tray", closeToTray);
  }
  async function toggleAutostart(e: Event) {
    autostart = (e.target as HTMLInputElement).checked;
    try {
      if (autostart) await enable();
      else await disable();
    } catch (err) {
      settingsStatus = t("settingsAutostartError", { msg: errText(err) });
    }
  }
  async function onLocaleChange(e: Event) {
    const val = (e.target as HTMLSelectElement).value;
    langChoice = val;
    if (val === "system") {
      // Prefer the desktop's locale over the embedded WebView locale. On some
      // Linux systems WebKit reports English although the session is Turkish.
      const systemLocale = await api.getSystemLocale().catch(() => null);
      setLocale(normalizeLocale(systemLocale ?? navigator.language));
      await api.setSetting("locale", "system");
    } else {
      setLocale(val as LocaleCode);
      await api.setSetting("locale", val);
    }
  }
  async function saveDefaultSpeed() {
    const bytes = Math.max(0, Math.round(defaultSpeedKb)) * 1024;
    await api.setSetting("default_speed_limit", String(bytes));
  }
  async function restoreDefaults() {
    categories = await api.restoreDefaultCategories();
  }
  async function resetFolder(id: number) {
    await api.resetCategoryDir(id);
    categories = await api.listCategories();
  }
  async function saveDir(c: Category, e: Event) {
    c.dir = (e.target as HTMLInputElement).value;
    await api.saveCategory(c.name, c.dir, c.rules, c.priority);
  }
  async function browseDir(c: Category) {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") {
      c.dir = dir;
      categories = [...categories];
      await api.saveCategory(c.name, c.dir, c.rules, c.priority);
    }
  }
  const fmtTime = (m: number) =>
    `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
  const daysLabel = (mask: number) => DAYS.filter((_, i) => mask & (1 << i)).map((k) => t(k)).join(" ");
  const actionLabel = (a: string) => {
    const key = ACTIONS.find(([v]) => v === a)?.[1];
    return key ? t(key) : a;
  };

  function toggleDay(i: number) {
    const s = new Set(nDays);
    s.has(i) ? s.delete(i) : s.add(i);
    nDays = s;
  }
  async function addSchedule() {
    const parts = nTime.split(":").map((x) => parseInt(x, 10));
    // A cleared time field yields NaN → at_minute NaN → JSON null → the Rust
    // command (non-optional i64) rejects. Validate before invoking.
    if (parts.length !== 2 || parts.some((n) => Number.isNaN(n))) {
      schedError = t("scheduleTimeInvalid");
      return;
    }
    const [h, m] = parts;
    let mask = 0;
    for (const i of nDays) mask |= 1 << i;
    try {
      await api.saveSchedule({
        name: null,
        action: nAction,
        days_mask: mask,
        at_minute: h * 60 + m,
        speed_limit: nAction === "set_speed" ? parseInt(nSpeed, 10) : null,
        enabled: true,
      });
      schedError = "";
      schedules = await api.listSchedules();
    } catch (e) {
      schedError = errText(e);
    }
  }
  async function removeSchedule(id: number) {
    await api.deleteSchedule(id);
    schedules = await api.listSchedules();
  }
  async function toggleSchedule(s: Schedule) {
    await api.saveSchedule({
      id: s.id,
      name: s.name,
      action: s.action,
      days_mask: s.days_mask,
      at_minute: s.at_minute,
      speed_limit: s.speed_limit,
      enabled: !s.enabled,
    });
    schedules = await api.listSchedules();
  }

  function goToSection(section: SettingsSection, focus = false) {
    activeSection = section;
    void tick().then(() => {
      const target = drawer?.querySelector<HTMLElement>(`#settings-${section}`);
      if (!target) return;
      target.scrollIntoView({ block: "start", behavior: "smooth" });
      if (focus) target.focus({ preventScroll: true });
    });
  }

  function persistCustomConfirmation(enabled: boolean): Promise<void> {
    const write = customConfirmationWrite
      .catch(() => {})
      .then(() => api.setSetting("on_complete_command_confirmed", enabled ? "true" : "false"));
    customConfirmationWrite = write.catch(() => {});
    return write;
  }

  async function invalidateCustomCommand() {
    customCommandRevision += 1;
    ocCustomConfirmed = false;
    settingsStatus = "";
    // Disable the stored command immediately: changing a command should never
    // leave the previous one eligible to run while it awaits reconfirmation.
    try {
      await persistCustomConfirmation(false);
    } catch (e) {
      settingsStatus = t("settingsSaveError", { msg: errText(e) });
    }
  }

  async function onCustomConfirmationChange() {
    if (!ocCustomConfirmed) {
      await invalidateCustomCommand();
      return;
    }
    await saveOnComplete();
  }
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<div class="drawer" bind:this={drawer} role="dialog" aria-modal="true" aria-labelledby="set-h" tabindex="-1" use:trapFocus={{ onEscape: onclose }}>
  <div class="dhead">
    <h2 id="set-h">{t("settings")}</h2>
    <button class="icon-btn" aria-label={t("close")} onclick={onclose}><Icon name="close" size={18} /></button>
  </div>
  {#if settingsStatus}<p class="hint settings-status" role="status">{settingsStatus}</p>{/if}

  <nav class="settings-nav" aria-label={t("settingsNavigation")}>
    {#each SECTION_LINKS as [section, label]}
      <button
        type="button"
        class:active={activeSection === section}
        aria-current={activeSection === section ? "page" : undefined}
        onclick={() => goToSection(section)}
      >{t(label)}</button>
    {/each}
  </nav>

  <section class="section" id="settings-general" tabindex="-1" onfocusin={() => (activeSection = "general")}>
    <h3 id="settings-general-heading">{t("sectGeneral")}</h3>
    <div class="srow">
      <span>{t("optAutoOrganize")}</span>
      <label class="switch"><input type="checkbox" checked={autoOrganize} onchange={toggleOrganize} aria-label={t("optAutoOrganize")} /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>{t("optClipboard")}</span>
      <label class="switch"><input type="checkbox" checked={clipboardWatch} onchange={toggleClipboard} aria-label={t("optClipboard")} /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>{t("optCloseTray")}</span>
      <label class="switch"><input type="checkbox" checked={closeToTray} onchange={toggleCloseToTray} aria-label={t("optCloseTray")} /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>{t("optAutostart")}</span>
      <label class="switch"><input type="checkbox" checked={autostart} onchange={toggleAutostart} aria-label={t("optAutostart")} /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>{t("theme")}</span>
      <select value={theme} onchange={onThemeChange} aria-label={t("theme")}>
        <option value="system">{t("themeSystem")}</option>
        <option value="light">{t("themeLight")}</option>
        <option value="dark">{t("themeDark")}</option>
      </select>
    </div>
    <div class="srow">
      <span>{t("onComplete")}</span>
      <select value={onComplete} onchange={onCompleteChange} aria-label={t("onComplete")}>
        {#each OC_ACTIONS as [v, l]}<option value={v}>{t(l)}</option>{/each}
      </select>
    </div>
    {#if onComplete === "custom"}
      <div class="custom-command">
        <input
          type="text"
          bind:value={ocCustomCmd}
          oninput={invalidateCustomCommand}
          placeholder={t("ocCustomPlaceholder")}
          aria-label={t("ocCustom")}
          style="font-family:var(--font-mono); font-size:0.8rem"
        />
        <p class="hint custom-command-warning">{t("ocCustomWarning")}</p>
        <label class="custom-command-confirm">
          <input type="checkbox" bind:checked={ocCustomConfirmed} onchange={onCustomConfirmationChange} />
          <span>{t("ocCustomConfirm")}</span>
        </label>
      </div>
    {/if}
  </section>

  <section class="section" id="settings-network" tabindex="-1" onfocusin={() => (activeSection = "network")}>
    <h3 id="settings-network-heading">{t("sectNetwork")}</h3>
    <div class="srow">
      <span>{t("optProxy")}</span>
      <input
        type="text"
        bind:value={proxy}
        onchange={saveProxy}
        placeholder={t("proxyPlaceholder")}
        aria-label={t("optProxy")}
        style="width:220px; font-family:var(--font-mono); font-size:0.8rem"
      />
    </div>
    {#if proxyStatus}<p class="hint" style="color:var(--error-fg)">{proxyStatus}</p>{/if}
    <div class="srow">
      <span>{t("optHandleMagnets")}</span>
      <label class="switch"><input type="checkbox" checked={handleMagnets} onchange={(e) => { handleMagnets = (e.target as HTMLInputElement).checked; setBool("handle_magnets", handleMagnets); }} aria-label={t("optHandleMagnets")} /><span class="track"></span></label>
    </div>
    <p class="hint">{t("optHandleMagnetsHint")}</p>
    <div class="srow">
      <span>{t("optDht")}</span>
      <label class="switch"><input type="checkbox" checked={dhtEnabled} onchange={(e) => { dhtEnabled = (e.target as HTMLInputElement).checked; setBool("dht_enabled", dhtEnabled); }} aria-label={t("optDht")} /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>{t("optSandbox")}</span>
      <label class="switch"><input type="checkbox" checked={sandboxChildren} onchange={(e) => { sandboxChildren = (e.target as HTMLInputElement).checked; setBool("sandbox_children", sandboxChildren); }} aria-label={t("optSandbox")} /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>{t("optBlockPrivate")}</span>
      <label class="switch"><input type="checkbox" checked={blockPrivateIps} onchange={(e) => { blockPrivateIps = (e.target as HTMLInputElement).checked; setBool("block_private_ips", blockPrivateIps); }} aria-label={t("optBlockPrivate")} /><span class="track"></span></label>
    </div>
    <p class="hint">{t("restartHint")}</p>
  </section>

  <section class="section" id="settings-language" tabindex="-1" onfocusin={() => (activeSection = "language")}>
    <h3 id="settings-language-heading">{t("sectLanguage")}</h3>
    <div class="srow">
      <span>{t("sectLanguage")}</span>
      <select value={langChoice} onchange={onLocaleChange} aria-label={t("sectLanguage")}>
        <option value="system">{t("langSystem")}</option>
        {#each Object.entries(LOCALES) as [code, meta]}<option value={code}>{meta.name}</option>{/each}
      </select>
    </div>
  </section>

  <section class="section" id="settings-connections" tabindex="-1" onfocusin={() => (activeSection = "connections")}>
    <h3 id="settings-connections-heading">{t("sectConnections")}</h3>
    <div class="srow">
      <span>{t("optConnPerServer")}</span>
      <span class="fmt-mono" style="color:var(--muted)">{connections}</span>
    </div>
    <input type="range" min="1" max="16" bind:value={connections} onchange={saveEngine} aria-label={t("optConnPerServer")} />
    <div class="srow">
      <span>{t("optSegments")}</span>
      <span class="fmt-mono" style="color:var(--muted)">{split}</span>
    </div>
    <input type="range" min="1" max="32" bind:value={split} onchange={saveEngine} aria-label={t("optSegments")} />
    <div class="srow">
      <span>{t("maxConcurrent")}</span>
      <span class="fmt-mono" style="color:var(--muted)">{maxConcurrent}</span>
    </div>
    <input type="range" min="1" max="20" bind:value={maxConcurrent} onchange={onMaxConcurrent} aria-label={t("maxConcurrent")} />
    <div class="srow">
      <span>{t("defaultSpeedLimit")}</span>
      <input type="number" min="0" bind:value={defaultSpeedKb} onchange={saveDefaultSpeed} style="width:110px" aria-label={t("defaultSpeedLimit")} />
    </div>
    <p class="hint">{t("connHint")}</p>
  </section>

  <section class="section extensions-section" id="settings-extensions" bind:this={extensionsSection} tabindex="-1" onfocusin={() => (activeSection = "extensions")}>
    <h3 id="settings-extensions-heading">{t("sectExtensions")}</h3>
    <p class="hint">{t("extensionStoreHint")}</p>
    <div class="extension-store-list">
      {#if showFirefoxStore}
        <div class="extension-store">
          <div>
            <div class="extension-store-heading">
              <strong>Firefox</strong>
              <span
                class="browser-status"
                class:connected={firefoxState === "browserStateConnected"}
                class:installed={firefoxState === "browserStateInstalled"}
                class:attention={firefoxState === "browserStateNeedsExtension"}
              >{t(firefoxState)}</span>
            </div>
            <p class="hint">{t("firefoxFamilyHint")}</p>
          </div>
          <button class="btn btn-primary" onclick={() => openUrl(STORE_URLS.firefox)}>
            <Icon name="link" size={16} /> {t("getForFirefox")}
          </button>
        </div>
      {/if}
      {#if showChromiumStore}
        <div class="extension-store">
          <div>
            <div class="extension-store-heading">
              <strong>Chrome</strong>
              <span
                class="browser-status"
                class:connected={chromiumState === "browserStateConnected"}
                class:installed={chromiumState === "browserStateInstalled"}
                class:attention={chromiumState === "browserStateNeedsExtension"}
              >{t(chromiumState)}</span>
            </div>
            <p class="hint">{t("chromiumFamilyHint")}</p>
          </div>
          <button class="btn btn-primary" onclick={() => openUrl(STORE_URLS.chrome)}>
            <Icon name="link" size={16} /> {t("getForChrome")}
          </button>
        </div>
      {/if}
    </div>
    <p class="hint">{t("extensionStoreOpenNote")}</p>
  </section>

  <section class="section" id="settings-scheduler" tabindex="-1" onfocusin={() => (activeSection = "scheduler")}>
    <h3 id="settings-scheduler-heading">{t("sectScheduler")}</h3>
    {#each schedules as s (s.id)}
      <div class="cat">
        <Icon name="clock" size={16} />
        <span class="hint" style="flex:1; opacity:{s.enabled ? 1 : 0.45}">
          {actionLabel(s.action)}{#if s.action === "set_speed" && s.speed_limit} · {Math.round(s.speed_limit / 1024)} KB/s{/if}
          · {fmtTime(s.at_minute)} · {daysLabel(s.days_mask)}
        </span>
        <label class="switch" title={s.enabled ? t("enabled") : t("disabled")}>
          <input type="checkbox" checked={s.enabled} onchange={() => toggleSchedule(s)} aria-label={t("enableSchedule")} />
          <span class="track"></span>
        </label>
        <button class="icon-btn danger" aria-label={t("deleteSchedule")} onclick={() => removeSchedule(s.id)}><Icon name="trash" size={16} /></button>
      </div>
    {/each}
    <div class="schedform">
      <select bind:value={nAction} aria-label={t("scheduleActionLabel")}>
        {#each ACTIONS as [v, l]}<option value={v}>{t(l)}</option>{/each}
      </select>
      <input type="time" bind:value={nTime} aria-label={t("scheduleTimeLabel")} />
      {#if nAction === "set_speed"}
        <input type="number" bind:value={nSpeed} aria-label={t("scheduleSpeedLabel")} style="width:110px" />
      {/if}
      <div class="days" role="group" aria-label={t("scheduleDaysLabel")}>
        {#each DAYS as d, i}
          <button type="button" class="tag day" class:on={nDays.has(i)} aria-pressed={nDays.has(i)} onclick={() => toggleDay(i)}>{t(d)}</button>
        {/each}
      </div>
      <button class="btn" onclick={addSchedule}><Icon name="add" size={16} /> {t("schedAddRule")}</button>
    </div>
    {#if schedError}<p class="sched-error" role="alert">{schedError}</p>{/if}
  </section>

  <section class="section" id="settings-categories" tabindex="-1" onfocusin={() => (activeSection = "categories")}>
    <h3 id="settings-categories-heading">{t("sectCategories")}</h3>
    {#each categories as c (c.id)}
      <div class="cat">
        <strong>{c.name}</strong>
        <input value={c.dir} onchange={(e) => saveDir(c, e)} aria-label={t("categoryFolderFor", { name: c.name })} />
        <input
          class="cat-prio"
          type="number"
          value={c.priority}
          onchange={(e) => savePriority(c, e)}
          title={t("catPriority")}
          aria-label="{t('catPriority')} {c.name}"
        />
        <button class="icon-btn" title={t("browseFolder")} aria-label={t("chooseFolderFor", { name: c.name })} onclick={() => browseDir(c)}><Icon name="folder" size={16} /></button>
        <button class="icon-btn" title={t("resetFolder")} aria-label="{t('resetFolder')} {c.name}" onclick={() => resetFolder(c.id)}><Icon name="retry" size={16} /></button>
        <button class="icon-btn danger" aria-label={t("deleteCategoryFor", { name: c.name })} onclick={() => removeCategory(c.id)}><Icon name="trash" size={16} /></button>
      </div>
    {/each}
    <div class="cat-form">
      <p class="hint cat-form-title">{t("addCategory")}</p>
      <div class="cat-form-row">
        <input bind:value={ncName} placeholder={t("catName")} aria-label={t("catName")} style="width:130px" />
        <input bind:value={ncDir} placeholder={t("catFolder")} aria-label={t("catFolder")} style="flex:1" />
        <button class="icon-btn" title={t("browseFolder")} aria-label={t("browseFolder")} onclick={browseNewDir}><Icon name="folder" size={16} /></button>
      </div>
      <input bind:value={ncRules} placeholder={t("catRules")} aria-label={t("catRules")} />
      <input bind:value={ncMime} placeholder={t("catMimeRules")} aria-label={t("catMimeRules")} />
      <input bind:value={ncHost} placeholder={t("catHostRules")} aria-label={t("catHostRules")} />
      <div class="btn-row">
        <button class="btn" onclick={addCategory}><Icon name="add" size={16} /> {t("addCategory")}</button>
        <button class="btn btn-ghost" onclick={restoreDefaults}><Icon name="retry" size={16} /> {t("restoreDefaults")}</button>
      </div>
    </div>
  </section>

  <section class="section" id="settings-updates" tabindex="-1" onfocusin={() => (activeSection = "updates")}>
    <h3 id="settings-updates-heading">{t("sectUpdates")}</h3>
    <button class="btn" onclick={checkForUpdates}><Icon name="download" size={16} /> {t("updateCheck")}</button>
    {#if updateStatus}<p class="hint">{updateStatus}</p>{/if}
    {#if update?.newer}
      <button class="btn btn-primary" onclick={installUpdate}>{update.can_install ? t("updateInstall") : t("updateView")}</button>
    {/if}
  </section>

  <footer class="drawer-foot">
    <button class="sponsor" onclick={() => openUrl(SPONSOR_URL)}>
      <Icon name="heart" size={15} />
      <span>{t("sponsor")}</span>
    </button>
  </footer>
</div>
