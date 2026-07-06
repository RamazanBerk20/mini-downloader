<script lang="ts">
  import { onMount } from "svelte";
  import { api, on } from "./api";
  import type { Download, Tick } from "./types";

  let url = $state("");
  let error = $state("");
  let filter = $state<string>("all");
  let items = $state<Download[]>([]);
  let globalSpeed = $state("0");

  const FILTERS = ["all", "active", "paused", "complete", "error"];
  const SPEEDS: [string, string][] = [
    ["0", "Unlimited"],
    ["512000", "500 KB/s"],
    ["1048576", "1 MB/s"],
    ["5242880", "5 MB/s"],
    ["10485760", "10 MB/s"],
  ];

  async function refresh() {
    const s = filter === "all" ? undefined : filter;
    try {
      items = await api.list(s);
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    const subs: Promise<() => void>[] = [];

    subs.push(
      on<{ updates: Tick[] }>("downloads:tick", (p) => {
        const idx = new Map(items.map((d, i) => [d.gid, i] as const));
        let changed = false;
        for (const u of p.updates) {
          const i = idx.get(u.gid);
          if (i === undefined) continue;
          const d = items[i];
          d.completed_bytes = u.completed;
          d.total_bytes = u.total;
          d.download_speed = u.dl_speed;
          d.upload_speed = u.ul_speed;
          d.connections = u.connections;
          d.num_seeders = u.num_seeders;
          if (!d.filename && u.name) d.filename = u.name;
          if (d.status !== "active") d.status = "active";
          changed = true;
        }
        if (changed) items = [...items];
      }),
    );

    const refetch = () => refresh();
    subs.push(on("downloads:state", refetch));
    subs.push(on("downloads:complete", refetch));
    subs.push(on("downloads:error", refetch));
    subs.push(on("downloads:reconciled", refetch));

    return () => subs.forEach((u) => u.then((f) => f()));
  });

  // Initial load + reload when the filter changes.
  $effect(() => {
    filter;
    refresh();
  });

  async function add(e: Event) {
    e.preventDefault();
    error = "";
    const u = url.trim();
    if (!u) return;
    try {
      await api.add(u);
      url = "";
      await refresh();
    } catch (err) {
      error = String(err);
    }
  }

  async function applyGlobalSpeed() {
    try {
      await api.setGlobalSpeed(parseInt(globalSpeed, 10), 0);
    } catch (e) {
      error = String(e);
    }
  }

  async function act(fn: () => Promise<unknown>) {
    try {
      await fn();
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  function name(d: Download): string {
    return d.filename || d.url.split("/").pop() || d.url;
  }
  function pct(d: Download): number {
    return d.total_bytes > 0 ? Math.round((d.completed_bytes / d.total_bytes) * 100) : 0;
  }
  function fmt(n: number): string {
    const u = ["B", "KB", "MB", "GB", "TB"];
    let v = n,
      i = 0;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(i === 0 ? 0 : 1)} ${u[i]}`;
  }
  function eta(d: Download): string {
    if (d.status !== "active" || d.download_speed <= 0 || d.total_bytes <= 0) return "";
    const left = d.total_bytes - d.completed_bytes;
    const s = Math.round(left / d.download_speed);
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
    return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  }
</script>

<header>
  <form onsubmit={add}>
    <input placeholder="Paste a URL or magnet link…" bind:value={url} />
    <button type="submit">Add</button>
  </form>
  <div class="toolbar">
    <div class="filters">
      {#each FILTERS as f}
        <button class="chip" class:on={filter === f} onclick={() => (filter = f)}>{f}</button>
      {/each}
    </div>
    <div class="tools">
      <button class="ghost" onclick={() => act(api.pauseAll)}>Pause all</button>
      <button class="ghost" onclick={() => act(api.resumeAll)}>Resume all</button>
      <label class="speed">
        ⤓
        <select bind:value={globalSpeed} onchange={applyGlobalSpeed}>
          {#each SPEEDS as [v, label]}<option value={v}>{label}</option>{/each}
        </select>
      </label>
    </div>
  </div>
</header>

{#if error}<p class="err">{error} <button class="x" onclick={() => (error = "")}>✕</button></p>{/if}

{#if items.length === 0}
  <p class="empty">No downloads. Paste a URL above to start.</p>
{:else}
  <ul>
    {#each items as d (d.id)}
      <li class="status-{d.status}">
        <div class="top">
          <span class="name" title={d.url}>{name(d)}</span>
          <span class="badge">{d.status}</span>
        </div>
        <progress max="100" value={pct(d)}></progress>
        <div class="bottom">
          <span class="meta">
            {fmt(d.completed_bytes)} / {d.total_bytes > 0 ? fmt(d.total_bytes) : "?"} · {pct(d)}%
            {#if d.status === "active"}· {fmt(d.download_speed)}/s{#if eta(d)} · {eta(d)} left{/if}
              {#if d.connections > 0}· {d.connections} conns{/if}{/if}
            {#if d.status === "error" && d.error_message}· <span class="ered">{d.error_message}</span>{/if}
          </span>
          <span class="actions">
            {#if d.status === "active" || d.status === "waiting"}
              <button onclick={() => act(() => api.pause(d.id))}>Pause</button>
            {:else if d.status === "paused"}
              <button onclick={() => act(() => api.resume(d.id))}>Resume</button>
            {/if}
            {#if d.status === "complete"}
              <button onclick={() => act(() => api.openFolder(d.id))}>Open folder</button>
            {/if}
            <button class="danger" onclick={() => act(() => api.remove(d.id, false))}>Remove</button>
          </span>
        </div>
      </li>
    {/each}
  </ul>
{/if}
