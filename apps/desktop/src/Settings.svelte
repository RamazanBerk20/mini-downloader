<script lang="ts">
  import { onMount } from "svelte";
  import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
  import { api } from "./api";
  import { trapFocus } from "./lib/a11y";
  import Icon from "./lib/Icon.svelte";
  import type { Category, Schedule } from "./types";

  let { onclose }: { onclose: () => void } = $props();

  let autoOrganize = $state(true);
  let clipboardWatch = $state(false);
  let closeToTray = $state(true);
  let autostart = $state(false);
  let categories = $state<Category[]>([]);
  let schedules = $state<Schedule[]>([]);
  let browserStatus = $state("");

  const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
  const ACTIONS = [
    ["pause_all", "Pause all"],
    ["resume_all", "Resume all"],
    ["set_speed", "Set speed limit"],
  ];

  let nAction = $state("pause_all");
  let nTime = $state("22:00");
  let nDays = $state<Set<number>>(new Set([0, 1, 2, 3, 4, 5, 6]));
  let nSpeed = $state("512000");

  onMount(async () => {
    autoOrganize = (await api.getSetting("auto_organize")) !== "false";
    clipboardWatch = (await api.getSetting("clipboard_watch")) === "true";
    closeToTray = (await api.getSetting("close_to_tray")) !== "false";
    categories = await api.listCategories();
    schedules = await api.listSchedules();
    try {
      autostart = await isEnabled();
    } catch {}
  });

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
      browserStatus = "Autostart error: " + String(err);
    }
  }
  async function saveDir(c: Category, e: Event) {
    c.dir = (e.target as HTMLInputElement).value;
    await api.saveCategory(c.name, c.dir, c.rules, c.priority);
  }
  async function installBrowser() {
    try {
      browserStatus = "Installed: " + (await api.installBrowser());
    } catch (e) {
      browserStatus = "Error: " + String(e);
    }
  }

  const fmtTime = (m: number) =>
    `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
  const daysLabel = (mask: number) => DAYS.filter((_, i) => mask & (1 << i)).join(" ");
  const actionLabel = (a: string) => ACTIONS.find(([v]) => v === a)?.[1] ?? a;

  function toggleDay(i: number) {
    const s = new Set(nDays);
    s.has(i) ? s.delete(i) : s.add(i);
    nDays = s;
  }
  async function addSchedule() {
    const [h, m] = nTime.split(":").map((x) => parseInt(x, 10));
    let mask = 0;
    for (const i of nDays) mask |= 1 << i;
    await api.saveSchedule({
      name: null,
      action: nAction,
      days_mask: mask,
      at_minute: h * 60 + m,
      speed_limit: nAction === "set_speed" ? parseInt(nSpeed, 10) : null,
      enabled: true,
    });
    schedules = await api.listSchedules();
  }
  async function removeSchedule(id: number) {
    await api.deleteSchedule(id);
    schedules = await api.listSchedules();
  }
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<div class="drawer" role="dialog" aria-modal="true" aria-labelledby="set-h" tabindex="-1" use:trapFocus={{ onEscape: onclose }}>
  <div class="dhead">
    <h2 id="set-h">Settings</h2>
    <button class="icon-btn" aria-label="Close settings" onclick={onclose}><Icon name="close" size={18} /></button>
  </div>

  <section class="section">
    <h3>General</h3>
    <div class="srow">
      <span>Auto-organize finished files</span>
      <label class="switch"><input type="checkbox" checked={autoOrganize} onchange={toggleOrganize} aria-label="Auto-organize finished files" /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>Watch clipboard for links</span>
      <label class="switch"><input type="checkbox" checked={clipboardWatch} onchange={toggleClipboard} aria-label="Watch clipboard for links" /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>Close to tray (keep running)</span>
      <label class="switch"><input type="checkbox" checked={closeToTray} onchange={toggleCloseToTray} aria-label="Close to tray" /><span class="track"></span></label>
    </div>
    <div class="srow">
      <span>Start on login (minimized)</span>
      <label class="switch"><input type="checkbox" checked={autostart} onchange={toggleAutostart} aria-label="Start on login" /><span class="track"></span></label>
    </div>
  </section>

  <section class="section">
    <h3>Firefox integration</h3>
    <button class="btn" onclick={installBrowser}><Icon name="link" size={16} /> Install native-messaging host</button>
    {#if browserStatus}<p class="hint">{browserStatus}</p>{/if}
    <p class="hint">Load the extension from <code>extension/</code> via <code>about:debugging</code>.</p>
  </section>

  <section class="section">
    <h3>Scheduler</h3>
    {#each schedules as s (s.id)}
      <div class="cat">
        <Icon name="clock" size={16} />
        <span class="hint" style="flex:1">
          {actionLabel(s.action)}{#if s.action === "set_speed" && s.speed_limit} · {Math.round(s.speed_limit / 1024)} KB/s{/if}
          · {fmtTime(s.at_minute)} · {daysLabel(s.days_mask)}
        </span>
        <button class="icon-btn danger" aria-label="Delete schedule" onclick={() => removeSchedule(s.id)}><Icon name="trash" size={16} /></button>
      </div>
    {/each}
    <div class="schedform">
      <select bind:value={nAction} aria-label="Schedule action">
        {#each ACTIONS as [v, l]}<option value={v}>{l}</option>{/each}
      </select>
      <input type="time" bind:value={nTime} aria-label="Schedule time" />
      {#if nAction === "set_speed"}
        <input type="number" bind:value={nSpeed} aria-label="Speed limit in bytes per second" style="width:110px" />
      {/if}
      <div class="days" role="group" aria-label="Days">
        {#each DAYS as d, i}
          <button type="button" class="tag day" class:on={nDays.has(i)} aria-pressed={nDays.has(i)} onclick={() => toggleDay(i)}>{d}</button>
        {/each}
      </div>
      <button class="btn" onclick={addSchedule}><Icon name="add" size={16} /> Add rule</button>
    </div>
  </section>

  <section class="section">
    <h3>Categories</h3>
    {#each categories as c (c.id)}
      <div class="cat">
        <strong>{c.name}</strong>
        <input value={c.dir} onchange={(e) => saveDir(c, e)} aria-label="{c.name} folder" />
      </div>
    {/each}
  </section>
</div>
