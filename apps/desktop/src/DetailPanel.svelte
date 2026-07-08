<script lang="ts">
  import { onMount } from "svelte";
  import { api, errText } from "./api";
  import { t } from "./lib/i18n.svelte";
  import type { Download, DownloadDetails } from "./types";

  let { d, onact }: { d: Download; onact: (fn: () => Promise<unknown>) => void } = $props();

  let details = $state<DownloadDetails | null>(null);
  let error = $state("");
  // File-selection edits staged locally until "Apply".
  let picked = $state<Set<number>>(new Set());
  let dirty = $state(false);

  const isTorrent = $derived(d.kind === "torrent" || d.kind === "magnet");

  async function load() {
    try {
      const det = await api.getDetails(d.id);
      details = det;
      error = "";
      if (!dirty) picked = new Set(det.files.filter((f) => f.selected).map((f) => f.index));
    } catch (e) {
      error = errText(e);
    }
  }

  onMount(() => {
    load();
    const iv = setInterval(load, 1500);
    return () => clearInterval(iv);
  });

  function toggleFile(index: number) {
    const s = new Set(picked);
    s.has(index) ? s.delete(index) : s.add(index);
    picked = s;
    dirty = true;
  }

  function applySelection() {
    // Keep the staged edit until the apply actually succeeds — a failure must
    // not let the next poll wipe the user's selection.
    onact(async () => {
      await api.setTorrentFiles(d.id, [...picked]);
      dirty = false;
    });
  }

  function base(p: string): string {
    return p.split("/").pop() || p;
  }
  function pct(done: number, total: number): number {
    return total > 0 ? Math.round((done / total) * 100) : 0;
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
</script>

<li class="detail-panel">
  {#if error}
    <p class="hint" style="color:var(--error-fg)">{error}</p>
  {:else if !details}
    <p class="hint">…</p>
  {:else}
    <div class="detail-meta">
      <span class="dm-label">URL</span><span class="dm-value" title={details.url}>{details.url}</span>
      <span class="dm-label">{t("detailFolder")}</span><span class="dm-value" title={details.dir}>{details.dir}</span>
      {#if details.num_pieces > 0}
        <span class="dm-label">{t("detailPieces")}</span>
        <span class="dm-value">{details.num_pieces} × {fmt(details.piece_length)}</span>
      {/if}
      {#if details.error_message}
        <span class="dm-label">{t("statusFailed")}</span><span class="dm-value err-text">{details.error_message}</span>
      {/if}
    </div>

    {#if details.files.length > 0 && (details.files.length > 1 || isTorrent)}
      <h4 class="detail-h">
        {t("detailFiles")}
        {#if isTorrent}
          <span class="hint" style="margin:0">{t("filesSelected", { n: picked.size, m: details.files.length })}</span>
        {/if}
      </h4>
      <div class="detail-files">
        {#each details.files as f (f.index)}
          <div class="df-row">
            {#if isTorrent}
              <input
                type="checkbox"
                checked={picked.has(f.index)}
                onchange={() => toggleFile(f.index)}
                aria-label={base(f.path)}
              />
            {/if}
            <span class="df-name" title={f.path}>{base(f.path) || t("fetchingMeta")}</span>
            <div class="progress df-progress"><span style="width:{pct(f.completed_length, f.length)}%"></span></div>
            <span class="df-size">{fmt(f.completed_length)} / {fmt(f.length)}</span>
          </div>
        {/each}
      </div>
      {#if isTorrent && dirty}
        <button class="btn btn-primary detail-apply" disabled={picked.size === 0} onclick={applySelection}>
          {t("applySelection")}
        </button>
      {/if}
    {:else if isTorrent && details.files.length === 0}
      <p class="hint">{t("fetchingMeta")}</p>
    {/if}

    {#if isTorrent}
      <h4 class="detail-h">{t("detailPeers")}</h4>
      {#if details.peers.length === 0}
        <p class="hint">{t("noPeers")}</p>
      {:else}
        <div class="detail-peers">
          {#each details.peers as p, i (i)}
            <span class="dp-ip">{p.ip}</span>
            <span class="dp-speed">↓ {fmt(p.down_speed)}/s</span>
            <span class="dp-speed">↑ {fmt(p.up_speed)}/s</span>
            <span class="dp-seed">{p.seeder ? t("peerSeed") : t("peerLeech")}</span>
          {/each}
        </div>
      {/if}
    {/if}
  {/if}
</li>
