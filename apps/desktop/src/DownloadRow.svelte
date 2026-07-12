<script lang="ts">
  import { api } from "./api";
  import Icon from "./lib/Icon.svelte";
  import { currentLocale, t, type MsgKey } from "./lib/i18n.svelte";
  import type { IconName } from "./lib/icons";
  import type { Download } from "./types";

  const STATUS_KEY: Record<string, MsgKey> = {
    active: "statusActive",
    waiting: "statusWaiting",
    paused: "statusPaused",
    complete: "statusCompleted",
    error: "statusFailed",
    queued: "statusQueued",
    scheduled: "statusScheduled",
  };

  let {
    d,
    i,
    onact,
    selected = false,
    onselect,
    onmenu,
    onreorder,
    grouped = false,
    expanded = false,
    ondetails,
  }: {
    d: Download;
    i: number;
    onact: (fn: () => Promise<unknown>) => void;
    selected?: boolean;
    onselect?: (id: number, e: MouseEvent) => void;
    onmenu?: (d: Download, x: number, y: number) => void;
    onreorder?: (srcId: number, targetId: number) => void;
    grouped?: boolean;
    expanded?: boolean;
    ondetails?: (id: number) => void;
  } = $props();

  // Per-download speed cap presets (bytes/sec; 0 = unlimited).
  const SPEED_PRESETS = [0, 262144, 524288, 1048576, 2097152, 5242880, 10485760];
  function fmtSpeedOpt(v: number): string {
    return v === 0 ? t("unlimited") : `${fmt(v)}/s`;
  }
  async function copyUrl() {
    try {
      await navigator.clipboard.writeText(d.url);
    } catch {}
  }

  // Per-download speed: presets + a Custom… entry that reveals a KB/s field.
  let showCustom = $state(false);
  let customKb = $state("");
  function onSpeedChange(e: Event) {
    const v = parseInt((e.currentTarget as HTMLSelectElement).value, 10);
    if (v === -1) {
      customKb = String(Math.round((d.speed_limit ?? 0) / 1024));
      showCustom = true;
      return;
    }
    onact(() => api.setDownloadSpeed(d.id, v));
  }
  function applyCustom() {
    const kb = parseInt(customKb, 10);
    showCustom = false;
    if (!Number.isNaN(kb) && kb >= 0) onact(() => api.setDownloadSpeed(d.id, kb * 1024));
  }

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
  const isTorrent = $derived(d.kind === "magnet" || d.kind === "torrent");
  const isAria2 = $derived(!(d.kind === "video" || d.kind === "hls" || d.kind === "dash"));
  // No known total while active → show an indeterminate bar, not a dead 0%.
  const indeterminate = $derived(d.status === "active" && d.total_bytes <= 0);
  const completedWithoutSize = $derived(
    d.status === "complete" && d.total_bytes <= 0 && d.completed_bytes <= 0,
  );

  function onKey(e: KeyboardEvent) {
    if (e.target !== e.currentTarget) return;
    if (e.key === " ") {
      e.preventDefault();
      if (isRunning) onact(() => api.pause(d.id));
      else if (d.status === "paused" || d.status === "scheduled") onact(() => api.resume(d.id));
    } else if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      onact(() => api.remove(d.id, false));
    } else if (e.key === "Enter" && d.status === "complete") {
      e.preventDefault();
      onact(() => api.openFolder(d.id));
    } else if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
      e.preventDefault();
      const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
      onmenu?.(d, rect.left + Math.min(40, rect.width / 2), rect.top + Math.min(36, rect.height / 2));
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<li
  class="row {d.status}"
  class:grouped
  style="--i:{i}"
  tabindex="0"
  aria-label={t("downloadRowLabel", { name: name(d), status: t(STATUS_KEY[d.status] ?? "statusActive"), pct: pct(d) })}
  onkeydown={onKey}
  oncontextmenu={(e) => { e.preventDefault(); onmenu?.(d, e.clientX, e.clientY); }}
  draggable={d.status === "waiting"}
  ondragstart={(e) => e.dataTransfer?.setData("text/minidl-id", String(d.id))}
  ondragover={(e) => { if (d.status === "waiting") e.preventDefault(); }}
  ondrop={(e) => {
    e.preventDefault();
    const src = e.dataTransfer?.getData("text/minidl-id");
    if (src && Number(src) !== d.id) onreorder?.(Number(src), d.id);
  }}
>
  <div class="row-head">
    {#if onselect}
      <input
        type="checkbox"
        class="row-check"
        checked={selected}
        aria-label={t("selectDownload", { name: name(d) })}
        onclick={(e) => onselect?.(d.id, e)}
      />
    {/if}
    <Icon name={fileIcon(d)} size={16} />
    <span class="row-name" title={d.url}>{name(d)}</span>
    <span class="badge {d.status}">{t(STATUS_KEY[d.status] ?? "statusActive")}</span>
  </div>

  <div
    class="progress"
    class:indeterminate
    role="progressbar"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={indeterminate ? undefined : pct(d)}
    aria-valuetext={indeterminate ? t("fetchingMeta") : `${pct(d)}%`}
    aria-label={t("downloadProgress")}
  >
    <span style="width:{indeterminate ? 30 : pct(d)}%"></span>
  </div>

  <div class="row-foot">
    <span class="row-meta">
      {#if indeterminate}
        {t("fetchingMeta")}
      {:else if completedWithoutSize}
        {t("statusCompleted")}
      {:else}
        {fmt(d.completed_bytes)} / {d.total_bytes > 0 ? fmt(d.total_bytes) : "—"} · {pct(d)}%
      {/if}
      {#if d.status === "active"}
        · {fmt(d.download_speed)}/s{#if eta(d)} · {eta(d)}{/if}{#if d.connections > 0} · {d.connections}c{/if}
        {#if isTorrent}· ↑{fmt(d.upload_speed)}/s{#if d.num_seeders > 0} · {d.num_seeders}⚲{/if}{/if}
      {/if}
      {#if d.status === "error" && d.error_message}· <span class="err-text">{d.error_message}</span>{/if}
      {#if d.status === "scheduled" && d.start_at}· {new Date(d.start_at * 1000).toLocaleString(currentLocale())}{/if}
      {#if d.checksum && d.status === "complete"}· <span class="verified-tag">{t("verifiedTag")}</span>{/if}
    </span>

    <span class="row-actions">
      {#if d.status === "waiting"}
        <button class="icon-btn" title={t("moveUp")} aria-label={t("moveUp")} onclick={() => onact(() => api.moveInQueue(d.id, "up"))}>
          <Icon name="chevron-up" size={15} />
        </button>
        <button class="icon-btn" title={t("moveDown")} aria-label={t("moveDown")} onclick={() => onact(() => api.moveInQueue(d.id, "down"))}>
          <Icon name="chevron-down" size={15} />
        </button>
      {/if}
      {#if isRunning && isAria2}
        {#if showCustom}
          <input
            class="speed-sel"
            type="number"
            min="0"
            placeholder="KB/s"
            bind:value={customKb}
            onkeydown={(e) => { if (e.key === "Enter") applyCustom(); }}
            onblur={applyCustom}
            aria-label="{t('speedLimit')} {name(d)}"
          />
        {:else}
          <select
            class="speed-sel"
            title={t("speedLimit")}
            aria-label="{t('speedLimit')} {name(d)}"
            value={d.speed_limit ?? 0}
            onchange={onSpeedChange}
          >
            {#each SPEED_PRESETS as v}<option value={v}>{fmtSpeedOpt(v)}</option>{/each}
            <option value={-1}>{t("speedCustom")}</option>
          </select>
        {/if}
      {/if}
      {#if isRunning}
        <button class="icon-btn" title={t("pause")} aria-label="{t('pause')} {name(d)}" onclick={() => onact(() => api.pause(d.id))}>
          <Icon name="pause" size={16} />
        </button>
      {:else if d.status === "paused" || d.status === "scheduled"}
        <button class="icon-btn" title={t("resume")} aria-label="{t('resume')} {name(d)}" onclick={() => onact(() => api.resume(d.id))}>
          <Icon name="play" size={16} />
        </button>
      {/if}
      {#if d.status === "complete"}
        <button class="icon-btn" title={t("openFolder")} aria-label="{t('openFolder')} {name(d)}" onclick={() => onact(() => api.openFolder(d.id))}>
          <Icon name="folder" size={16} />
        </button>
      {/if}
      {#if d.status === "error"}
        <button class="icon-btn" title={t("retry")} aria-label="{t('retry')} {name(d)}" onclick={() => onact(() => api.retry(d.id))}>
          <Icon name="retry" size={16} />
        </button>
      {/if}
      {#if ondetails}
        <button
          class="icon-btn"
          title={t("detailTitle")}
          aria-label="{t('detailTitle')} {name(d)}"
          aria-expanded={expanded}
          onclick={() => ondetails?.(d.id)}
        >
          <Icon name={expanded ? "chevron-up" : "chevron-down"} size={16} />
        </button>
      {/if}
      <button class="icon-btn" title={t("copyUrl")} aria-label="{t('copyUrl')} {name(d)}" onclick={copyUrl}>
        <Icon name="link" size={16} />
      </button>
      <button class="icon-btn danger" title={t("remove")} aria-label="{t('remove')} {name(d)}" onclick={() => onact(() => api.remove(d.id, false))}>
        <Icon name="trash" size={16} />
      </button>
    </span>
  </div>
</li>
