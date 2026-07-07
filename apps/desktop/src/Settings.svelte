<script lang="ts">
  import { onMount } from "svelte";
  import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
  import { open } from "@tauri-apps/plugin-dialog";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api, errText } from "./api";

  const SPONSOR_URL = "https://github.com/sponsors/RamazanBerk20";
  import { trapFocus } from "./lib/a11y";
  import Icon from "./lib/Icon.svelte";
  import type { Category, Schedule, UpdateInfo } from "./types";
  import { t, LOCALES, setLocale, normalizeLocale, type LocaleCode, type MsgKey } from "./lib/i18n.svelte";

  // Store listing URLs — filled by the maintainer after publishing to AMO /
  // Chrome Web Store (buttons only render when set). See scripts/EXTENSION-PUBLISHING.md.
  const STORE_URLS = { firefox: "", chrome: "" };
  const RELEASE_URL = "https://github.com/RamazanBerk20/mini-downloader/releases/latest";

  let { onclose }: { onclose: () => void } = $props();

  let autoOrganize = $state(true);
  let clipboardWatch = $state(false);
  let closeToTray = $state(true);
  let autostart = $state(false);
  let categories = $state<Category[]>([]);
  let schedules = $state<Schedule[]>([]);
  let browserStatus = $state("");
  let split = $state(16);
  let connections = $state(16);

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
  let updateStatus = $state("");
  let update = $state<UpdateInfo | null>(null);
  let langChoice = $state("system");
  let defaultSpeedKb = $state(0);

  const OC_ACTIONS: [string, MsgKey][] = [
    ["none", "ocNone"],
    ["quit", "ocQuit"],
    ["sleep", "ocSleep"],
    ["shutdown", "ocShutdown"],
  ];

  onMount(async () => {
    autoOrganize = (await api.getSetting("auto_organize")) !== "false";
    clipboardWatch = (await api.getSetting("clipboard_watch")) === "true";
    closeToTray = (await api.getSetting("close_to_tray")) !== "false";
    theme = (await api.getSetting("theme")) || "system";
    onComplete = (await api.getSetting("on_complete_action")) || "none";
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

  function applyTheme(v: string) {
    const root = document.documentElement;
    if (v === "light" || v === "dark") root.dataset.theme = v;
    else delete root.dataset.theme;
  }
  async function onThemeChange(e: Event) {
    theme = (e.target as HTMLSelectElement).value;
    applyTheme(theme);
    await api.setSetting("theme", theme);
  }
  async function onCompleteChange(e: Event) {
    onComplete = (e.target as HTMLSelectElement).value;
    await api.setSetting("on_complete_action", onComplete);
  }
  async function onMaxConcurrent(e: Event) {
    maxConcurrent = parseInt((e.target as HTMLInputElement).value, 10);
    await api.setMaxConcurrent(maxConcurrent);
  }
  async function addCategory() {
    if (!ncName.trim() || !ncDir.trim()) return;
    const rules = JSON.stringify(
      ncRules.split(",").map((s) => s.trim()).filter(Boolean),
    );
    await api.saveCategory(ncName.trim(), ncDir.trim(), rules, 0);
    ncName = "";
    ncDir = "";
    ncRules = "";
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
      browserStatus = "Engine error: " + errText(e);
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
      browserStatus = "Autostart error: " + errText(err);
    }
  }
  async function onLocaleChange(e: Event) {
    const val = (e.target as HTMLSelectElement).value;
    langChoice = val;
    if (val === "system") {
      setLocale(normalizeLocale(navigator.language));
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
  async function installBrowser() {
    try {
      browserStatus = "Installed: " + (await api.installBrowser());
    } catch (e) {
      browserStatus = "Error: " + errText(e);
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
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<div class="drawer" role="dialog" aria-modal="true" aria-labelledby="set-h" tabindex="-1" use:trapFocus={{ onEscape: onclose }}>
  <div class="dhead">
    <h2 id="set-h">{t("settings")}</h2>
    <button class="icon-btn" aria-label={t("close")} onclick={onclose}><Icon name="close" size={18} /></button>
  </div>

  <section class="section">
    <h3>{t("sectGeneral")}</h3>
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
  </section>

  <section class="section">
    <h3>{t("sectLanguage")}</h3>
    <div class="srow">
      <span>{t("sectLanguage")}</span>
      <select value={langChoice} onchange={onLocaleChange} aria-label={t("sectLanguage")}>
        <option value="system">{t("langSystem")}</option>
        {#each Object.entries(LOCALES) as [code, meta]}<option value={code}>{meta.name}</option>{/each}
      </select>
    </div>
  </section>

  <section class="section">
    <h3>{t("sectConnections")}</h3>
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

  <section class="section">
    <h3>{t("sectBrowser")}</h3>
    {#if STORE_URLS.firefox || STORE_URLS.chrome}
      <div class="srow" style="justify-content:flex-start; gap:0.5rem">
        {#if STORE_URLS.firefox}<button class="btn btn-primary" onclick={() => openUrl(STORE_URLS.firefox)}>{t("getForFirefox")}</button>{/if}
        {#if STORE_URLS.chrome}<button class="btn btn-primary" onclick={() => openUrl(STORE_URLS.chrome)}>{t("getForChrome")}</button>{/if}
      </div>
    {/if}
    <button class="btn" onclick={installBrowser}><Icon name="link" size={16} /> {t("installHost")}</button>
    {#if browserStatus}<p class="hint">{browserStatus}</p>{/if}
    <p class="hint">{t("browserHint")}</p>
    <button class="btn btn-ghost" onclick={() => openUrl(RELEASE_URL)}>{t("installGuide")}</button>
  </section>

  <section class="section">
    <h3>{t("sectScheduler")}</h3>
    {#each schedules as s (s.id)}
      <div class="cat">
        <Icon name="clock" size={16} />
        <span class="hint" style="flex:1; opacity:{s.enabled ? 1 : 0.45}">
          {actionLabel(s.action)}{#if s.action === "set_speed" && s.speed_limit} · {Math.round(s.speed_limit / 1024)} KB/s{/if}
          · {fmtTime(s.at_minute)} · {daysLabel(s.days_mask)}
        </span>
        <label class="switch" title={s.enabled ? "Enabled" : "Disabled"}>
          <input type="checkbox" checked={s.enabled} onchange={() => toggleSchedule(s)} aria-label="Enable this schedule" />
          <span class="track"></span>
        </label>
        <button class="icon-btn danger" aria-label="Delete schedule" onclick={() => removeSchedule(s.id)}><Icon name="trash" size={16} /></button>
      </div>
    {/each}
    <div class="schedform">
      <select bind:value={nAction} aria-label="Schedule action">
        {#each ACTIONS as [v, l]}<option value={v}>{t(l)}</option>{/each}
      </select>
      <input type="time" bind:value={nTime} aria-label="Schedule time" />
      {#if nAction === "set_speed"}
        <input type="number" bind:value={nSpeed} aria-label="Speed limit in bytes per second" style="width:110px" />
      {/if}
      <div class="days" role="group" aria-label="Days">
        {#each DAYS as d, i}
          <button type="button" class="tag day" class:on={nDays.has(i)} aria-pressed={nDays.has(i)} onclick={() => toggleDay(i)}>{t(d)}</button>
        {/each}
      </div>
      <button class="btn" onclick={addSchedule}><Icon name="add" size={16} /> {t("schedAddRule")}</button>
    </div>
    {#if schedError}<p class="sched-error" role="alert">{schedError}</p>{/if}
  </section>

  <section class="section">
    <h3>{t("sectCategories")}</h3>
    {#each categories as c (c.id)}
      <div class="cat">
        <strong>{c.name}</strong>
        <input value={c.dir} onchange={(e) => saveDir(c, e)} aria-label="{c.name} folder" />
        <button class="icon-btn" title={t("browseFolder")} aria-label="Choose folder for {c.name}" onclick={() => browseDir(c)}><Icon name="folder" size={16} /></button>
        <button class="icon-btn" title={t("resetFolder")} aria-label="{t('resetFolder')} {c.name}" onclick={() => resetFolder(c.id)}><Icon name="retry" size={16} /></button>
        <button class="icon-btn danger" aria-label="Delete {c.name}" onclick={() => removeCategory(c.id)}><Icon name="trash" size={16} /></button>
      </div>
    {/each}
    <div class="schedform">
      <input bind:value={ncName} placeholder={t("catName")} aria-label={t("catName")} style="width:110px" />
      <input bind:value={ncDir} placeholder={t("catFolder")} aria-label={t("catFolder")} />
      <button class="icon-btn" title={t("browseFolder")} aria-label={t("browseFolder")} onclick={browseNewDir}><Icon name="folder" size={16} /></button>
      <input bind:value={ncRules} placeholder={t("catRules")} aria-label={t("catRules")} />
      <button class="btn" onclick={addCategory}><Icon name="add" size={16} /> {t("addCategory")}</button>
    </div>
    <button class="btn btn-ghost" onclick={restoreDefaults}><Icon name="retry" size={16} /> {t("restoreDefaults")}</button>
  </section>

  <section class="section">
    <h3>{t("sectUpdates")}</h3>
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
