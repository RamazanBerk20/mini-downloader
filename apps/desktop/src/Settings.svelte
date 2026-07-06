<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "./api";
  import type { Category, Schedule } from "./types";

  let { onclose }: { onclose: () => void } = $props();

  let autoOrganize = $state(true);
  let clipboardWatch = $state(false);
  let categories = $state<Category[]>([]);
  let schedules = $state<Schedule[]>([]);
  let browserStatus = $state("");

  const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
  const ACTIONS = [
    ["pause_all", "Pause all"],
    ["resume_all", "Resume all"],
    ["set_speed", "Set speed limit"],
  ];

  // New-schedule form
  let nAction = $state("pause_all");
  let nTime = $state("22:00");
  let nDays = $state<Set<number>>(new Set([0, 1, 2, 3, 4, 5, 6]));
  let nSpeed = $state("512000");

  onMount(async () => {
    autoOrganize = (await api.getSetting("auto_organize")) !== "false";
    clipboardWatch = (await api.getSetting("clipboard_watch")) === "true";
    categories = await api.listCategories();
    schedules = await api.listSchedules();
  });

  async function toggleOrganize(e: Event) {
    autoOrganize = (e.target as HTMLInputElement).checked;
    await api.setSetting("auto_organize", autoOrganize ? "true" : "false");
  }
  async function toggleClipboard(e: Event) {
    clipboardWatch = (e.target as HTMLInputElement).checked;
    await api.setClipboardWatch(clipboardWatch);
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

  function fmtTime(min: number): string {
    return `${String(Math.floor(min / 60)).padStart(2, "0")}:${String(min % 60).padStart(2, "0")}`;
  }
  function daysLabel(mask: number): string {
    return DAYS.filter((_, i) => mask & (1 << i)).join(",");
  }
  function toggleDay(i: number) {
    const s = new Set(nDays);
    if (s.has(i)) s.delete(i);
    else s.add(i);
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
  function actionLabel(a: string): string {
    return ACTIONS.find(([v]) => v === a)?.[1] ?? a;
  }
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<aside class="drawer">
  <div class="dhead"><h2>Settings</h2><button onclick={onclose}>✕</button></div>

  <section>
    <label class="srow">
      <span>Auto-organize finished files</span>
      <input type="checkbox" checked={autoOrganize} onchange={toggleOrganize} />
    </label>
    <label class="srow">
      <span>Watch clipboard for links</span>
      <input type="checkbox" checked={clipboardWatch} onchange={toggleClipboard} />
    </label>
  </section>

  <section>
    <h3>Firefox integration</h3>
    <button onclick={installBrowser}>Install native-messaging host</button>
    {#if browserStatus}<p class="hint">{browserStatus}</p>{/if}
    <p class="hint">Load the extension from <code>extension/</code> via <code>about:debugging</code>.</p>
  </section>

  <section>
    <h3>Scheduler</h3>
    {#each schedules as s (s.id)}
      <div class="cat">
        <span class="hint" style="flex:1">
          {actionLabel(s.action)}{#if s.action === "set_speed" && s.speed_limit} ({Math.round(s.speed_limit / 1024)} KB/s){/if}
          · {fmtTime(s.at_minute)} · {daysLabel(s.days_mask)}
        </span>
        <button class="danger" onclick={() => removeSchedule(s.id)}>✕</button>
      </div>
    {/each}
    <div class="schedform">
      <select bind:value={nAction}>
        {#each ACTIONS as [v, l]}<option value={v}>{l}</option>{/each}
      </select>
      <input type="time" bind:value={nTime} />
      {#if nAction === "set_speed"}
        <input type="number" bind:value={nSpeed} title="bytes/sec" style="width:110px" />
      {/if}
      <div class="days">
        {#each DAYS as d, i}
          <button class="chip" class:on={nDays.has(i)} onclick={() => toggleDay(i)}>{d}</button>
        {/each}
      </div>
      <button onclick={addSchedule}>Add rule</button>
    </div>
  </section>

  <section>
    <h3>Categories</h3>
    {#each categories as c (c.id)}
      <div class="cat">
        <strong>{c.name}</strong>
        <input value={c.dir} onchange={(e) => saveDir(c, e)} />
      </div>
    {/each}
  </section>
</aside>
