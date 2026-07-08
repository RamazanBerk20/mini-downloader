<script lang="ts">
  import { onMount } from "svelte";
  import { api, errText } from "./api";
  import { t } from "./lib/i18n.svelte";
  import { trapFocus } from "./lib/a11y";
  import Icon from "./lib/Icon.svelte";
  import type { Format, MediaInfo, MediaOpts } from "./types";

  let { onclose }: { onclose: () => void } = $props();

  let url = $state("");
  let loading = $state(false);
  let error = $state("");
  let info = $state<MediaInfo | null>(null);
  let playlist = $state(false);
  let playlistTouched = $state(false);
  // Selection by entry index — a playlist can contain the same URL twice.
  let checked = $state<Set<number>>(new Set());
  let quality = $state("");
  let adding = $state(false);

  // Media options (subtitles / audio extraction / thumbnail), persisted as the
  // `media_prefs` setting so the last-used choices stick.
  let optSubs = $state(false);
  let subLangs = $state("en");
  let embedSubs = $state(false);
  let audioOnly = $state(false);
  let audioFormat = $state("mp3");
  let embedThumb = $state(false);

  const QUALITIES = [
    { value: "", key: "qBest" as const },
    { value: "bv*[height<=1080]+ba/b", key: "q1080" as const },
    { value: "bv*[height<=720]+ba/b", key: "q720" as const },
  ];

  onMount(async () => {
    try {
      const raw = await api.getSetting("media_prefs");
      if (raw) {
        const p = JSON.parse(raw) as Partial<MediaOpts>;
        optSubs = !!p.write_subs;
        if (p.sub_langs) subLangs = p.sub_langs;
        embedSubs = !!p.embed_subs;
        audioOnly = !!p.audio_only;
        if (p.audio_format) audioFormat = p.audio_format;
        embedThumb = !!p.embed_thumbnail;
      }
    } catch {}
  });

  function opts(): MediaOpts {
    return {
      write_subs: optSubs,
      sub_langs: subLangs.trim() || "en",
      embed_subs: embedSubs,
      audio_only: audioOnly,
      audio_format: audioFormat,
      embed_thumbnail: embedThumb,
    };
  }
  function savePrefs() {
    api.setSetting("media_prefs", JSON.stringify(opts())).catch(() => {});
  }

  // Suggest playlist probing when the URL looks like one (unless the user has
  // explicitly toggled it).
  $effect(() => {
    const u = url.toLowerCase();
    if (!playlistTouched) playlist = u.includes("list=") || u.includes("/playlist");
  });

  async function probe(e: Event) {
    e.preventDefault();
    error = "";
    info = null;
    const u = url.trim();
    if (!u) return;
    loading = true;
    try {
      info = await api.probeMedia(u, playlist);
      if (info.kind === "playlist") checked = new Set(info.entries.map((_, i) => i));
    } catch (err) {
      error = errText(err);
    }
    loading = false;
  }

  async function grab(fmt?: string) {
    savePrefs();
    try {
      await api.addMedia(url.trim(), fmt, opts());
      onclose();
    } catch (err) {
      error = errText(err);
    }
  }

  function toggleEntry(i: number) {
    const s = new Set(checked);
    s.has(i) ? s.delete(i) : s.add(i);
    checked = s;
  }
  function toggleAll() {
    if (!info) return;
    checked = checked.size === info.entries.length ? new Set() : new Set(info.entries.map((_, i) => i));
  }

  async function addPlaylist() {
    if (!info || adding) return;
    const entries = info.entries
      .filter((_, i) => checked.has(i))
      .map((e) => ({ url: e.url, title: e.title }));
    if (!entries.length) return;
    savePrefs();
    adding = true;
    try {
      await api.addPlaylistBatch(entries, info.title, quality || undefined, opts());
      onclose();
    } catch (err) {
      error = errText(err);
    }
    adding = false;
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
  function fmtDur(s: number): string {
    if (!s) return "—";
    const m = Math.floor(s / 60);
    if (m >= 60) return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
    return `${m}:${String(s % 60).padStart(2, "0")}`;
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
    <h2 id="media-h">{t("grabVideoTitle")}</h2>
    <button class="icon-btn" aria-label={t("close")} onclick={onclose}><Icon name="close" size={18} /></button>
  </div>

  <form class="addbar" style="padding:0" onsubmit={probe}>
    <input placeholder={t("videoUrlPlaceholder")} bind:value={url} aria-label="Video URL" />
    <button class="btn btn-primary" type="submit" disabled={loading}>{loading ? t("probing") : t("probe")}</button>
  </form>
  <label class="media-opt media-playlist-toggle">
    <input type="checkbox" bind:checked={playlist} onchange={() => (playlistTouched = true)} />
    {t("plPlaylist")}
  </label>

  <div class="media-opts">
    <label class="media-opt">
      <input type="checkbox" bind:checked={optSubs} />
      {t("optSubs")}
    </label>
    {#if optSubs}
      <input class="media-langs" type="text" bind:value={subLangs} placeholder="en,tr" aria-label={t("optSubLangs")} title={t("optSubLangs")} />
      <label class="media-opt">
        <input type="checkbox" bind:checked={embedSubs} />
        {t("optEmbedSubs")}
      </label>
    {/if}
    <label class="media-opt">
      <input type="checkbox" bind:checked={audioOnly} />
      {t("optAudioOnly")}
    </label>
    {#if audioOnly}
      <select class="media-fmt" bind:value={audioFormat} aria-label={t("optAudioFormat")}>
        <option value="mp3">mp3</option>
        <option value="m4a">m4a</option>
        <option value="opus">opus</option>
      </select>
    {/if}
    <label class="media-opt">
      <input type="checkbox" bind:checked={embedThumb} />
      {t("optEmbedThumb")}
    </label>
  </div>

  {#if error}<p class="hint" style="color:var(--error-fg)">{error}</p>{/if}

  {#if info && info.kind === "playlist"}
    <p style="margin:0.9rem 0 0.4rem; font-weight:500;">{info.title}</p>
    <div class="pl-toolbar">
      <span class="hint">{t("plEntries", { n: info.entries.length })}</span>
      <button class="btn btn-ghost" onclick={toggleAll}>{t("plSelectAll")}</button>
      {#if !audioOnly}
        <select class="media-fmt" bind:value={quality} aria-label={t("plQuality")} title={t("plQuality")}>
          {#each QUALITIES as q (q.value)}<option value={q.value}>{t(q.key)}</option>{/each}
        </select>
      {/if}
    </div>
    <div class="linklist">
      {#each info.entries as e, i (i)}
        <label class="linkrow">
          <input type="checkbox" checked={checked.has(i)} onchange={() => toggleEntry(i)} aria-label={e.title} />
          <span class="pl-idx">{i + 1}</span>
          <span class="u pl-title" title={e.url}>{e.title}</span>
          <span class="pl-dur">{fmtDur(e.duration)}</span>
        </label>
      {/each}
    </div>
    <button class="btn btn-primary" style="margin-top:0.8rem" disabled={adding || checked.size === 0} onclick={addPlaylist}>
      <Icon name="download" size={16} /> {t("plAddSelected", { n: checked.size })}
    </button>
  {:else if info}
    <p style="margin:0.9rem 0 0.4rem; font-weight:500;">{info.title}</p>
    <button class="btn btn-primary" onclick={() => grab()}><Icon name="download" size={16} /> {t("bestQuality")}</button>
    <table class="fmt-table">
      <thead>
        <tr><th>{t("colQuality")}</th><th>{t("colFormat")}</th><th>{t("colCodec")}</th><th>{t("colSize")}</th><th></th></tr>
      </thead>
      <tbody>
        {#each info.formats as f (f.format_id)}
          <tr>
            <td><span class="tag">{f.resolution}</span></td>
            <td class="fmt-mono">{f.ext}</td>
            <td class="fmt-mono">{codec(f)}</td>
            <td class="fmt-mono">{fmtSize(f.filesize)}</td>
            <td><button class="icon-btn" aria-label="Grab {f.resolution} {f.ext}" title={t("grab")} onclick={() => grab(f.format_id)}><Icon name="download" size={16} /></button></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>
