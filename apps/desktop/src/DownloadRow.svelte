<script lang="ts">
  import { api } from "./api";
  import Icon from "./lib/Icon.svelte";
  import type { IconName } from "./lib/icons";
  import type { Download } from "./types";

  let {
    d,
    i,
    onact,
  }: { d: Download; i: number; onact: (fn: () => Promise<unknown>) => void } = $props();

  function fileIcon(d: Download): IconName {
    if (d.kind === "video" || d.kind === "hls" || d.kind === "dash") return "video";
    if (d.kind === "magnet" || d.kind === "torrent") return "magnet";
    return "file";
  }
  function name(d: Download): string {
    return d.filename || d.url.split("/").pop() || d.url;
  }
  function pct(d: Download): number {
    if (d.status === "complete") return 100;
    return d.total_bytes > 0 ? Math.round((d.completed_bytes / d.total_bytes) * 100) : 0;
  }
  function fmt(n: number): string {
    const u = ["B", "KB", "MB", "GB", "TB"];
    let v = n,
      k = 0;
    while (v >= 1024 && k < u.length - 1) {
      v /= 1024;
      k++;
    }
    return `${v.toFixed(k === 0 ? 0 : 1)} ${u[k]}`;
  }
  function eta(d: Download): string {
    if (d.status !== "active" || d.download_speed <= 0 || d.total_bytes <= 0) return "";
    const s = Math.round((d.total_bytes - d.completed_bytes) / d.download_speed);
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  }

  const isRunning = $derived(d.status === "active" || d.status === "waiting");

  function onKey(e: KeyboardEvent) {
    if (e.target !== e.currentTarget) return;
    if (e.key === " ") {
      e.preventDefault();
      if (isRunning) onact(() => api.pause(d.id));
      else if (d.status === "paused") onact(() => api.resume(d.id));
    } else if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      onact(() => api.remove(d.id, false));
    } else if (e.key === "Enter" && d.status === "complete") {
      e.preventDefault();
      onact(() => api.openFolder(d.id));
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<li
  class="row {d.status}"
  style="--i:{i}"
  tabindex="0"
  aria-label="{name(d)}, {d.status}, {pct(d)} percent"
  onkeydown={onKey}
>
  <div class="row-head">
    <Icon name={fileIcon(d)} size={16} />
    <span class="row-name" title={d.url}>{name(d)}</span>
    <span class="badge {d.status}">{d.status}</span>
  </div>

  <div
    class="progress"
    role="progressbar"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={pct(d)}
    aria-label="Download progress"
  >
    <span style="width:{pct(d)}%"></span>
  </div>

  <div class="row-foot">
    <span class="row-meta">
      {fmt(d.completed_bytes)} / {d.total_bytes > 0 ? fmt(d.total_bytes) : "—"} · {pct(d)}%
      {#if d.status === "active"}
        · {fmt(d.download_speed)}/s{#if eta(d)} · {eta(d)}{/if}{#if d.connections > 0} · {d.connections}c{/if}
      {/if}
      {#if d.status === "error" && d.error_message}· <span class="err-text">{d.error_message}</span>{/if}
    </span>

    <span class="row-actions">
      {#if isRunning}
        <button class="icon-btn" title="Pause" aria-label="Pause {name(d)}" onclick={() => onact(() => api.pause(d.id))}>
          <Icon name="pause" size={16} />
        </button>
      {:else if d.status === "paused"}
        <button class="icon-btn" title="Resume" aria-label="Resume {name(d)}" onclick={() => onact(() => api.resume(d.id))}>
          <Icon name="play" size={16} />
        </button>
      {/if}
      {#if d.status === "complete"}
        <button class="icon-btn" title="Open folder" aria-label="Open folder for {name(d)}" onclick={() => onact(() => api.openFolder(d.id))}>
          <Icon name="folder" size={16} />
        </button>
      {/if}
      {#if d.status === "error"}
        <button class="icon-btn" title="Retry" aria-label="Retry {name(d)}" onclick={() => onact(async () => { await api.remove(d.id, false); await api.add(d.url); })}>
          <Icon name="retry" size={16} />
        </button>
      {/if}
      <button class="icon-btn danger" title="Remove" aria-label="Remove {name(d)}" onclick={() => onact(() => api.remove(d.id, false))}>
        <Icon name="trash" size={16} />
      </button>
    </span>
  </div>
</li>
