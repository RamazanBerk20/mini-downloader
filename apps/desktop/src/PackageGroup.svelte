<script lang="ts">
  import { api } from "./api";
  import Icon from "./lib/Icon.svelte";
  import { t } from "./lib/i18n.svelte";
  import type { Download, Package } from "./types";

  let {
    pkg,
    items,
    collapsed,
    ontoggle,
    onact,
  }: {
    pkg: Package;
    items: Download[];
    collapsed: boolean;
    ontoggle: (id: number) => void;
    onact: (fn: () => Promise<unknown>) => void;
  } = $props();

  // Aggregates over the visible members (the ones passed in after filtering).
  const total = $derived(items.reduce((s, d) => s + Math.max(0, d.total_bytes), 0));
  const done = $derived(items.reduce((s, d) => s + Math.max(0, d.completed_bytes), 0));
  const speed = $derived(
    items.reduce((s, d) => s + (d.status === "active" ? d.download_speed : 0), 0),
  );
  const pct = $derived.by(() => {
    if (items.every((d) => d.status === "complete")) return 100;
    return total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
  });
  const anyRunning = $derived(items.some((d) => d.status === "active" || d.status === "waiting"));
  const anyPaused = $derived(items.some((d) => d.status === "paused"));

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

  async function each(fn: (id: number) => Promise<unknown>) {
    for (const d of items) {
      try {
        await fn(d.id);
      } catch {}
    }
  }
</script>

<li class="pkg-head">
  <button
    class="pkg-toggle"
    aria-expanded={!collapsed}
    aria-label={collapsed ? t("pkgExpand") : t("pkgCollapse")}
    onclick={() => ontoggle(pkg.id)}
  >
    <Icon name={collapsed ? "chevron-right" : "chevron-down"} size={16} />
  </button>
  <div class="pkg-body">
    <div class="pkg-title">
      <span class="pkg-name" title={pkg.name}>{pkg.name}</span>
      <span class="pkg-count">{t("pkgItems", { n: items.length })}</span>
    </div>
    <div class="progress pkg-progress" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct} aria-label={pkg.name}>
      <span style="width:{pct}%"></span>
    </div>
    <span class="pkg-meta">
      {pct}%{#if speed > 0} · {fmt(speed)}/s{/if}{#if total > 0} · {fmt(done)} / {fmt(total)}{/if}
    </span>
  </div>
  <span class="row-actions">
    {#if anyRunning}
      <button class="icon-btn" title={t("pause")} aria-label="{t('pause')} {pkg.name}" onclick={() => onact(() => each((id) => api.pause(id)))}>
        <Icon name="pause" size={16} />
      </button>
    {/if}
    {#if anyPaused}
      <button class="icon-btn" title={t("resume")} aria-label="{t('resume')} {pkg.name}" onclick={() => onact(() => each((id) => api.resume(id)))}>
        <Icon name="play" size={16} />
      </button>
    {/if}
    <button class="icon-btn danger" title={t("remove")} aria-label="{t('remove')} {pkg.name}" onclick={() => onact(() => each((id) => api.remove(id, false)))}>
      <Icon name="trash" size={16} />
    </button>
  </span>
</li>
