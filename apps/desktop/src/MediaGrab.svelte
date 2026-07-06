<script lang="ts">
  import { api } from "./api";
  import type { MediaInfo } from "./types";

  let { onclose }: { onclose: () => void } = $props();

  let url = $state("");
  let loading = $state(false);
  let error = $state("");
  let info = $state<MediaInfo | null>(null);

  async function probe(e: Event) {
    e.preventDefault();
    error = "";
    info = null;
    const u = url.trim();
    if (!u) return;
    loading = true;
    try {
      info = await api.probeMedia(u);
    } catch (err) {
      error = String(err);
    }
    loading = false;
  }

  async function grab(fmt?: string) {
    try {
      await api.addMedia(url.trim(), fmt);
      onclose();
    } catch (err) {
      error = String(err);
    }
  }

  function fmtSize(n: number): string {
    if (!n) return "";
    const u = ["B", "KB", "MB", "GB"];
    let v = n,
      i = 0;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(1)} ${u[i]}`;
  }

  function label(f: MediaInfo["formats"][number]): string {
    const parts = [f.resolution, f.ext];
    if (f.note) parts.push(f.note);
    if (f.vcodec && f.vcodec !== "none") parts.push(f.vcodec.split(".")[0]);
    if (f.acodec && f.acodec !== "none" && f.vcodec === "none") parts.push("audio");
    const sz = fmtSize(f.filesize);
    if (sz) parts.push(sz);
    return parts.join(" · ");
  }
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<aside class="drawer">
  <div class="dhead"><h2>Grab video</h2><button onclick={onclose}>✕</button></div>

  <form onsubmit={probe}>
    <input placeholder="Video page URL (YouTube, etc.)…" bind:value={url} />
    <button type="submit" disabled={loading}>{loading ? "…" : "Probe"}</button>
  </form>

  {#if error}<p class="err">{error}</p>{/if}

  {#if info}
    <p style="margin:0.8rem 0 0.4rem"><strong>{info.title}</strong></p>
    <button onclick={() => grab()}>⬇ Best quality (auto)</button>
    <div style="margin-top:0.8rem">
      {#each info.formats as f (f.format_id)}
        <div class="cat">
          <span class="hint" style="flex:1">{label(f)}</span>
          <button onclick={() => grab(f.format_id)}>Grab</button>
        </div>
      {/each}
    </div>
  {/if}
</aside>
