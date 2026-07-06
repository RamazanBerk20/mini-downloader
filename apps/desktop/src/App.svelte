<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";

  type Tick = {
    gid: string;
    name: string;
    completed: number;
    total: number;
    speed: number;
    status: string;
  };

  let url = $state("");
  let error = $state("");
  let downloads = $state<Map<string, Tick>>(new Map());

  const list = $derived([...downloads.values()]);

  onMount(() => {
    const un = listen<{ updates: Tick[] }>("downloads:tick", (e) => {
      const next = new Map(downloads);
      for (const u of e.payload.updates) next.set(u.gid, u);
      downloads = next;
    });
    return () => {
      un.then((f) => f());
    };
  });

  async function add(e: Event) {
    e.preventDefault();
    error = "";
    const u = url.trim();
    if (!u) return;
    try {
      await invoke("add_download", { url: u });
      url = "";
    } catch (err) {
      error = String(err);
    }
  }

  function pct(d: Tick): number {
    return d.total > 0 ? Math.round((d.completed / d.total) * 100) : 0;
  }

  function fmt(n: number): string {
    const units = ["B", "KB", "MB", "GB", "TB"];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
  }
</script>

<main>
  <h1>Linux Download Manager</h1>

  <form onsubmit={add}>
    <input placeholder="Paste a URL…" bind:value={url} />
    <button type="submit">Download</button>
  </form>

  {#if error}<p class="err">{error}</p>{/if}

  {#if list.length === 0}
    <p class="empty">No downloads yet. Paste a URL above to start.</p>
  {:else}
    <ul>
      {#each list as d (d.gid)}
        <li>
          <div class="row">
            <span class="name">{d.name || d.gid}</span>
            <span class="status">{d.status}</span>
          </div>
          <progress max="100" value={pct(d)}></progress>
          <div class="meta">
            {fmt(d.completed)} / {fmt(d.total)} · {fmt(d.speed)}/s · {pct(d)}%
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</main>
