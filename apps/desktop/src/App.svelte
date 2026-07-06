<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, on } from "./api";
  import { announce, trapFocus } from "./lib/a11y";
  import type { Category, Download, Tick } from "./types";
  import Sidebar from "./Sidebar.svelte";
  import DownloadRow from "./DownloadRow.svelte";
  import Icon from "./lib/Icon.svelte";
  import Settings from "./Settings.svelte";
  import MediaGrab from "./MediaGrab.svelte";
  import LinkGrabber from "./LinkGrabber.svelte";

  let all = $state<Download[]>([]);
  let categories = $state<Category[]>([]);
  let statusFilter = $state("all");
  let categoryId = $state<number | null>(null);
  let search = $state("");
  let url = $state("");
  let globalSpeed = $state("0");
  let error = $state("");
  let clipboardUrl = $state<string | null>(null);
  let showSettings = $state(false);
  let showMedia = $state(false);
  let showGrabber = $state(false);
  let showHelp = $state(false);

  let addEl: HTMLInputElement;
  let searchEl: HTMLInputElement;

  const filtered = $derived.by(() => {
    let list = all;
    if (categoryId !== null) {
      list = list.filter((d) => d.category_id === categoryId);
    } else if (statusFilter !== "all") {
      list = list.filter((d) =>
        statusFilter === "active"
          ? d.status === "active" || d.status === "waiting"
          : d.status === statusFilter,
      );
    }
    const q = search.trim().toLowerCase();
    if (q) list = list.filter((d) => (d.filename || d.url).toLowerCase().includes(q));
    return list;
  });

  const completedCount = $derived(all.filter((d) => d.status === "complete").length);

  const pageTitle = $derived(
    categoryId !== null
      ? (categories.find((c) => c.id === categoryId)?.name ?? "Category")
      : ({ all: "All downloads", active: "Active", paused: "Paused", complete: "Completed", error: "Failed" }[
          statusFilter
        ] ?? "Downloads"),
  );

  async function refresh() {
    try {
      all = await api.list();
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    api.listCategories().then((c) => (categories = c)).catch(() => {});
    refresh();

    const subs: Promise<() => void>[] = [];
    subs.push(
      on<{ updates: Tick[] }>("downloads:tick", (p) => {
        const idx = new Map(all.map((d, i) => [d.id, i] as const));
        let changed = false;
        for (const u of p.updates) {
          const i = idx.get(u.id);
          if (i === undefined) continue;
          const d = all[i];
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
        if (changed) all = [...all];
      }),
    );
    subs.push(on("downloads:state", () => refresh()));
    subs.push(
      on<{ name?: string }>("downloads:complete", (p) => {
        announce(`Completed: ${p.name ?? "download"}`);
        refresh();
      }),
    );
    subs.push(
      on<{ message?: string }>("downloads:error", (p) => {
        announce(`Download failed${p.message ? ": " + p.message : ""}`);
        refresh();
      }),
    );
    subs.push(on("downloads:reconciled", () => refresh()));
    subs.push(on<{ url: string }>("clipboard:detected", (p) => (clipboardUrl = p.url)));

    window.addEventListener("keydown", onGlobalKey);
    return () => {
      subs.forEach((u) => u.then((f) => f()));
      window.removeEventListener("keydown", onGlobalKey);
    };
  });

  function setStatus(s: string) {
    statusFilter = s;
    categoryId = null;
  }
  function setCategory(id: number | null) {
    categoryId = id;
    statusFilter = "all";
  }

  function inField(t: EventTarget | null) {
    const el = t as HTMLElement | null;
    return !!el && ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName);
  }
  function onGlobalKey(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === "n") { e.preventDefault(); addEl?.focus(); return; }
    if (mod && e.key.toLowerCase() === "f") { e.preventDefault(); searchEl?.focus(); return; }
    if (mod && e.key === ",") { e.preventDefault(); showSettings = true; return; }
    if (mod && e.shiftKey && e.key.toLowerCase() === "p") { e.preventDefault(); act(api.pauseAll); return; }
    if (mod && e.shiftKey && e.key.toLowerCase() === "r") { e.preventDefault(); act(api.resumeAll); return; }
    if (inField(e.target)) return;
    if (e.key === "/") { e.preventDefault(); searchEl?.focus(); return; }
    if (e.key === "?") { e.preventDefault(); showHelp = !showHelp; return; }
    if (e.key >= "1" && e.key <= "5") {
      setStatus(["all", "active", "paused", "complete", "error"][+e.key - 1]);
    }
  }

  async function add(e: Event) {
    e.preventDefault();
    error = "";
    const u = url.trim();
    if (!u) return;
    try {
      await api.add(u);
      url = "";
      announce("Download added");
      await refresh();
    } catch (err) {
      error = String(err);
    }
  }

  async function pickFile() {
    error = "";
    const path = await open({
      multiple: false,
      filters: [{ name: "Torrent / Metalink", extensions: ["torrent", "meta4", "metalink"] }],
    });
    if (typeof path !== "string") return;
    try {
      if (path.endsWith(".torrent")) await api.addTorrentFile(path);
      else await api.addMetalinkFile(path);
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  async function addClipboard() {
    if (!clipboardUrl) return;
    const u = clipboardUrl;
    clipboardUrl = null;
    try {
      await api.add(u);
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  async function setSpeed(v: string) {
    globalSpeed = v;
    try {
      await api.setGlobalSpeed(parseInt(v, 10), 0);
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
</script>

<div class="shell">
  <Sidebar
    {all}
    {categories}
    {statusFilter}
    {categoryId}
    {globalSpeed}
    onStatus={setStatus}
    onCategory={setCategory}
    onSpeed={setSpeed}
    onSettings={() => (showSettings = true)}
  />

  <main class="main">
    <div class="topbar">
      <h2 class="page-title">{pageTitle}</h2>
      <div class="search">
        <Icon name="search" size={15} />
        <input type="search" placeholder="Search" bind:value={search} bind:this={searchEl} aria-label="Search downloads" />
      </div>
      <div class="head-actions">
        {#if completedCount > 0}
          <button class="btn btn-ghost" title="Remove completed downloads from the list" onclick={() => act(api.removeCompleted)}>
            <Icon name="check" size={15} /> Clear completed
          </button>
        {/if}
        <button class="icon-btn" title="Add .torrent / .metalink file" aria-label="Add torrent or metalink file" onclick={pickFile}>
          <Icon name="file" />
        </button>
        <button class="icon-btn" title="Grab video (yt-dlp)" aria-label="Grab video" onclick={() => (showMedia = true)}>
          <Icon name="video" />
        </button>
        <button class="icon-btn" title="Grab many links" aria-label="Grab links from text" onclick={() => (showGrabber = true)}>
          <Icon name="link" />
        </button>
        <button class="icon-btn" title="Keyboard shortcuts (?)" aria-label="Keyboard shortcuts" onclick={() => (showHelp = true)}>
          <Icon name="help" />
        </button>
      </div>
    </div>

    <form class="addbar" onsubmit={add}>
      <input placeholder="Paste a URL or magnet link" bind:value={url} bind:this={addEl} aria-label="Add download URL" />
      <button class="btn btn-primary" type="submit"><Icon name="add" size={16} /> Add</button>
    </form>

    {#if error}
      <div class="banner" role="alert">
        <span>{error}</span>
        <button class="icon-btn" aria-label="Dismiss" onclick={() => (error = "")}><Icon name="close" size={16} /></button>
      </div>
    {/if}

    <div class="main-scroll">
      {#if filtered.length === 0}
        <div class="empty">
          <Icon name="inbox" size={56} />
          {#if all.length === 0}
            <h2>No downloads yet</h2>
            <p>Paste a URL above, or capture one from Firefox.</p>
            <p class="keys"><kbd>Ctrl</kbd> <kbd>N</kbd> to add · <kbd>?</kbd> for shortcuts</p>
          {:else}
            <h2>Nothing here</h2>
            <p>No downloads match this filter.</p>
          {/if}
        </div>
      {:else}
        <ul class="dl-list" role="list">
          {#each filtered as d, i (d.id)}
            <DownloadRow {d} {i} onact={act} />
          {/each}
        </ul>
      {/if}
    </div>
  </main>
</div>

{#if showSettings}
  <Settings onclose={() => { showSettings = false; refresh(); }} />
{/if}
{#if showMedia}
  <MediaGrab onclose={() => { showMedia = false; refresh(); }} />
{/if}
{#if showGrabber}
  <LinkGrabber onclose={() => { showGrabber = false; refresh(); }} />
{/if}

{#if showHelp}
  <div class="overlay" onclick={() => (showHelp = false)} role="presentation"></div>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="help-h" tabindex="-1" use:trapFocus={{ onEscape: () => (showHelp = false) }}>
    <div class="dhead">
      <h2 id="help-h">Keyboard shortcuts</h2>
      <button class="icon-btn" aria-label="Close" onclick={() => (showHelp = false)}><Icon name="close" size={18} /></button>
    </div>
    <div class="shortcuts">
      <span class="k"><kbd>Ctrl</kbd><kbd>N</kbd></span><span class="d">Focus the add field</span>
      <span class="k"><kbd>/</kbd></span><span class="d">Search</span>
      <span class="k"><kbd>1</kbd>–<kbd>5</kbd></span><span class="d">Switch status filter</span>
      <span class="k"><kbd>Ctrl</kbd><kbd>,</kbd></span><span class="d">Settings</span>
      <span class="k"><kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>P</kbd></span><span class="d">Pause all</span>
      <span class="k"><kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>R</kbd></span><span class="d">Resume all</span>
      <span class="k"><kbd>Space</kbd></span><span class="d">Pause/resume focused download</span>
      <span class="k"><kbd>Del</kbd></span><span class="d">Remove focused download</span>
      <span class="k"><kbd>Enter</kbd></span><span class="d">Open folder (completed)</span>
      <span class="k"><kbd>Esc</kbd></span><span class="d">Close dialogs</span>
    </div>
  </div>
{/if}

{#if clipboardUrl}
  <div class="toast" role="status" aria-live="polite">
    <Icon name="download" size={18} />
    <span class="u">{clipboardUrl.length > 54 ? clipboardUrl.slice(0, 54) + "…" : clipboardUrl}</span>
    <button class="btn btn-primary" onclick={addClipboard}>Download</button>
    <button class="btn btn-ghost" onclick={() => (clipboardUrl = null)}>Dismiss</button>
  </div>
{/if}
