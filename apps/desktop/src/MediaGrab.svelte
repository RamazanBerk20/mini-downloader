<script lang="ts">
  import { api } from "./api";
  import { trapFocus } from "./lib/a11y";
  import Icon from "./lib/Icon.svelte";
  import type { Format, MediaInfo } from "./types";

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
    if (!n) return "—";
    const u = ["B", "KB", "MB", "GB"];
    let v = n,
      i = 0;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(1)} ${u[i]}`;
  }
  function codec(f: Format): string {
    if (f.vcodec && f.vcodec !== "none") return f.vcodec.split(".")[0];
    if (f.acodec && f.acodec !== "none") return "audio";
    return "—";
  }
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<div class="modal" role="dialog" aria-modal="true" aria-labelledby="media-h" tabindex="-1" use:trapFocus={{ onEscape: onclose }}>
  <div class="dhead">
    <h2 id="media-h">Grab video</h2>
    <button class="icon-btn" aria-label="Close" onclick={onclose}><Icon name="close" size={18} /></button>
  </div>

  <form class="addbar" style="padding:0" onsubmit={probe}>
    <input placeholder="Video page URL (YouTube, etc.)" bind:value={url} aria-label="Video URL" />
    <button class="btn btn-primary" type="submit" disabled={loading}>{loading ? "Probing…" : "Probe"}</button>
  </form>

  {#if error}<p class="hint" style="color:var(--error-fg)">{error}</p>{/if}

  {#if info}
    <p style="margin:0.9rem 0 0.4rem; font-weight:500;">{info.title}</p>
    <button class="btn btn-primary" onclick={() => grab()}><Icon name="download" size={16} /> Best quality</button>
    <table class="fmt-table">
      <thead>
        <tr><th>Quality</th><th>Format</th><th>Codec</th><th>Size</th><th></th></tr>
      </thead>
      <tbody>
        {#each info.formats as f (f.format_id)}
          <tr>
            <td><span class="tag">{f.resolution}</span></td>
            <td class="fmt-mono">{f.ext}</td>
            <td class="fmt-mono">{codec(f)}</td>
            <td class="fmt-mono">{fmtSize(f.filesize)}</td>
            <td><button class="icon-btn" aria-label="Grab {f.resolution} {f.ext}" title="Grab" onclick={() => grab(f.format_id)}><Icon name="download" size={16} /></button></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>
