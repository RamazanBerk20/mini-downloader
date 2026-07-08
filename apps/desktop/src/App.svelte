<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, on, errText } from "./api";
  import { announce, trapFocus } from "./lib/a11y";
  import type { Category, Download, Package, Tick, UpdateInfo } from "./types";
  import Sidebar from "./Sidebar.svelte";
  import DownloadRow from "./DownloadRow.svelte";
  import PackageGroup from "./PackageGroup.svelte";
  import Icon from "./lib/Icon.svelte";
  import Settings from "./Settings.svelte";
  import MediaGrab from "./MediaGrab.svelte";
  import LinkGrabber from "./LinkGrabber.svelte";
  import DetailPanel from "./DetailPanel.svelte";
  import { t } from "./lib/i18n.svelte";

  let all = $state<Download[]>([]);
  let packages = $state<Package[]>([]);
  let collapsedPkgs = $state<Set<number>>(new Set());
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
  let selected = $state<Set<number>>(new Set());
  let updateInfo = $state<UpdateInfo | null>(null);
  let expandedId = $state<number | null>(null);
  let scheduleFor = $state<Download | null>(null);
  let scheduleAt = $state("");
  let showAddOpts = $state(false);
  let addChecksum = $state("");

  function toggleDetails(id: number) {
    expandedId = expandedId === id ? null : id;
  }

  function openSchedule(d: Download) {
    // Default to one hour from now, in the datetime-local format.
    const dt = new Date(Date.now() + 3600_000);
    dt.setSeconds(0, 0);
    scheduleAt = new Date(dt.getTime() - dt.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
    scheduleFor = d;
  }
  async function saveScheduleAt() {
    if (!scheduleFor) return;
    const ts = Math.floor(new Date(scheduleAt).getTime() / 1000);
    if (Number.isNaN(ts)) return;
    const id = scheduleFor.id;
    scheduleFor = null;
    await act(() => api.scheduleDownload(id, ts));
  }

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
  const errorCount = $derived(all.filter((d) => d.status === "error").length);

  const pageTitle = $derived(
    categoryId !== null
      ? (categories.find((c) => c.id === categoryId)?.name ?? t("titleCategory"))
      : ({
          all: t("titleAll"),
          active: t("titleActive"),
          paused: t("titlePaused"),
          complete: t("titleCompleted"),
          error: t("titleFailed"),
          scheduled: t("titleScheduled"),
        }[statusFilter] ?? "Downloads"),
  );

  async function refresh() {
    try {
      all = await api.list();
      packages = await api.listPackages();
    } catch (e) {
      error = errText(e);
    }
  }

  // The flat filtered list regrouped for rendering: a package's first visible
  // member pulls the whole (visible) group in at that position; everything
  // else renders as a plain row.
  type ListItem =
    | { kind: "row"; d: Download }
    | { kind: "pkg"; pkg: Package; items: Download[] };
  const listItems = $derived.by(() => {
    const byPkg = new Map<number, Download[]>();
    for (const d of filtered) {
      if (d.package_id != null) {
        const arr = byPkg.get(d.package_id);
        if (arr) arr.push(d);
        else byPkg.set(d.package_id, [d]);
      }
    }
    const out: ListItem[] = [];
    const seen = new Set<number>();
    for (const d of filtered) {
      if (d.package_id == null) {
        out.push({ kind: "row", d });
        continue;
      }
      if (seen.has(d.package_id)) continue;
      seen.add(d.package_id);
      const pkg = packages.find((p) => p.id === d.package_id);
      const items = byPkg.get(d.package_id)!;
      if (pkg) out.push({ kind: "pkg", pkg, items });
      else for (const m of items) out.push({ kind: "row", d: m });
    }
    return out;
  });

  function togglePkg(id: number) {
    const s = new Set(collapsedPkgs);
    if (s.has(id)) s.delete(id);
    else s.add(id);
    collapsedPkgs = s;
  }

  // Patch a single row in place. Returns false if the id isn't loaded yet (a
  // brand-new download) so the caller can fall back to a full refresh. This
  // avoids re-fetching + re-rendering the whole list on every lifecycle event.
  function patchRow(id: number, changes: Partial<Download>): boolean {
    const i = all.findIndex((d) => d.id === id);
    if (i === -1) return false;
    Object.assign(all[i], changes);
    return true;
  }

  function toggleSelect(id: number) {
    const s = new Set(selected);
    if (s.has(id)) s.delete(id);
    else s.add(id);
    selected = s;
  }
  async function bulk(fn: (id: number) => Promise<unknown>) {
    for (const id of [...selected]) {
      try {
        await fn(id);
      } catch {}
    }
    selected = new Set();
    await refresh();
  }

  // Single shared right-click context menu (positioned at the cursor).
  let menu = $state<{ d: Download; x: number; y: number } | null>(null);
  function openMenu(d: Download, x: number, y: number) {
    menu = { d, x, y };
  }
  // Drag-reorder: move the dragged row to the drop target's slot in aria2's
  // waiting queue.
  async function reorder(srcId: number, targetId: number) {
    const waiting = all.filter((d) => d.status === "waiting");
    const pos = waiting.findIndex((d) => d.id === targetId);
    if (pos < 0) return;
    try {
      await api.setQueuePosition(srcId, pos);
    } catch {}
    await refresh();
  }

  async function doInstall() {
    if (!updateInfo) return;
    const info = updateInfo;
    updateInfo = null;
    try {
      await api.installUpdate(info.asset_url, info.url);
    } catch (e) {
      error = errText(e);
    }
  }

  onMount(() => {
    api.listCategories().then((c) => (categories = c)).catch(() => {});
    refresh();
    // Non-blocking update check against GitHub releases (no self-install on Linux).
    api.checkUpdate().then((u) => { if (u.newer) updateInfo = u; }).catch(() => {});

    const subs: Promise<() => void>[] = [];
    subs.push(
      on<{ updates: Tick[] }>("downloads:tick", (p) => {
        const idx = new Map(all.map((d, i) => [d.id, i] as const));
        let changed = false;
        for (const u of p.updates) {
          const i = idx.get(u.id);
          if (i === undefined) continue;
          const d = all[i];
          // Ignore stale ticks for rows the client no longer considers running
          // (e.g. a tick emitted just before a pause that lands after it) — don't
          // resurrect a paused/finished row to "active".
          if (d.status === "paused" || d.status === "scheduled" || d.status === "complete" || d.status === "error" || d.status === "removed") continue;
          d.completed_bytes = u.completed;
          d.total_bytes = u.total;
          d.download_speed = u.dl_speed;
          d.upload_speed = u.ul_speed;
          d.connections = u.connections;
          d.num_seeders = u.num_seeders;
          if (!d.filename && u.name) d.filename = u.name;
          d.status = "active";
          changed = true;
        }
        if (changed) all = [...all];
      }),
    );
    subs.push(
      on<{ id?: number; status?: string }>("downloads:state", (p) => {
        // A single transition patches in place; a new id (capture/deeplink) or a
        // batch payload needs a full reload.
        if (typeof p?.id === "number" && p.status) {
          if (!patchRow(p.id, { status: p.status as Download["status"] })) refresh();
        } else {
          refresh();
        }
      }),
    );
    subs.push(
      on<{ id?: number; name?: string }>("downloads:complete", (p) => {
        announce(t("announceCompleted", { name: p.name ?? "download" }));
        const changes: Partial<Download> = { status: "complete" };
        if (p.name) changes.filename = p.name;
        if (typeof p?.id !== "number" || !patchRow(p.id, changes)) refresh();
      }),
    );
    subs.push(
      on<{ id?: number; message?: string }>("downloads:error", (p) => {
        announce(p.message ? t("announceFailedDetail", { msg: p.message }) : t("announceFailed"));
        if (typeof p?.id !== "number" || !patchRow(p.id, { status: "error", error_message: p.message ?? null }))
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
    // A dialog is open → let it own the keyboard (its own focus trap handles
    // Escape). Otherwise single-key shortcuts (/, ?, 1–5) leak to the background
    // and pull focus out of the trapped modal.
    if (menu && e.key === "Escape") { menu = null; return; }
    if (showSettings || showMedia || showGrabber || showHelp) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === "n") { e.preventDefault(); addEl?.focus(); return; }
    if (mod && e.key.toLowerCase() === "f") { e.preventDefault(); searchEl?.focus(); return; }
    if (mod && e.key === ",") { e.preventDefault(); showSettings = true; return; }
    if (mod && e.shiftKey && e.key.toLowerCase() === "p") { e.preventDefault(); act(api.pauseAll); return; }
    if (mod && e.shiftKey && e.key.toLowerCase() === "r") { e.preventDefault(); act(api.resumeAll); return; }
    if (inField(e.target)) return;
    if (mod && e.key.toLowerCase() === "a") { e.preventDefault(); selected = new Set(filtered.map((d) => d.id)); return; }
    if (e.key === "/") { e.preventDefault(); searchEl?.focus(); return; }
    if (e.key === "?") { e.preventDefault(); showHelp = !showHelp; return; }
    if (e.key >= "1" && e.key <= "6") {
      setStatus(["all", "active", "paused", "scheduled", "complete", "error"][+e.key - 1]);
    }
  }

  async function add(e: Event) {
    e.preventDefault();
    error = "";
    const u = url.trim();
    if (!u) return;
    try {
      await api.add(u, addChecksum.trim() || undefined);
      url = "";
      addChecksum = "";
      showAddOpts = false;
      announce(t("announceAdded"));
      await refresh();
    } catch (err) {
      error = errText(err);
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
      error = errText(e);
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
      error = errText(e);
    }
  }

  async function setSpeed(v: string) {
    globalSpeed = v;
    try {
      await api.setGlobalSpeed(parseInt(v, 10), 0);
    } catch (e) {
      error = errText(e);
    }
  }

  async function act(fn: () => Promise<unknown>) {
    try {
      await fn();
      await refresh();
    } catch (e) {
      error = errText(e);
    }
  }

  async function setupBrowser() {
    error = "";
    try {
      const msg = await api.installBrowser();
      announce(msg);
    } catch (e) {
      error = errText(e);
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
        <input type="search" placeholder={t("search")} bind:value={search} bind:this={searchEl} aria-label="Search downloads" />
      </div>
      <div class="head-actions">
        {#if completedCount > 0}
          <button class="btn btn-ghost" title={t("clearCompleted")} onclick={() => act(api.removeCompleted)}>
            <Icon name="check" size={15} /> {t("clearCompleted")}
          </button>
        {/if}
        {#if errorCount > 0}
          <button class="btn btn-ghost" title={t("clearFailed")} onclick={() => act(api.removeFailed)}>
            <Icon name="warning" size={15} /> {t("clearFailed")}
          </button>
        {/if}
        <button class="icon-btn" title={t("tipAddFile")} aria-label={t("tipAddFile")} onclick={pickFile}>
          <Icon name="file" />
        </button>
        <button class="icon-btn" title={t("tipGrabVideo")} aria-label={t("tipGrabVideo")} onclick={() => (showMedia = true)}>
          <Icon name="video" />
        </button>
        <button class="icon-btn" title={t("tipGrabLinks")} aria-label={t("tipGrabLinks")} onclick={() => (showGrabber = true)}>
          <Icon name="link" />
        </button>
        <button class="icon-btn" title={t("tipShortcuts")} aria-label={t("tipShortcuts")} onclick={() => (showHelp = true)}>
          <Icon name="help" />
        </button>
      </div>
    </div>

    <form class="addbar" onsubmit={add}>
      <input placeholder={t("addPlaceholder")} bind:value={url} bind:this={addEl} aria-label="Add download URL" />
      <button
        class="icon-btn"
        type="button"
        title={t("checksumLabel")}
        aria-label={t("checksumLabel")}
        aria-expanded={showAddOpts}
        onclick={() => (showAddOpts = !showAddOpts)}
      >
        <Icon name={showAddOpts ? "chevron-up" : "chevron-down"} size={16} />
      </button>
      <button class="btn btn-primary" type="submit"><Icon name="add" size={16} /> {t("add")}</button>
    </form>
    {#if showAddOpts}
      <div class="add-opts">
        <label class="hint" for="add-checksum">{t("checksumLabel")}</label>
        <input
          id="add-checksum"
          type="text"
          bind:value={addChecksum}
          spellcheck="false"
          style="font-family:var(--font-mono); font-size:0.78rem; flex:1"
        />
      </div>
    {/if}

    {#if error}
      <div class="banner" role="alert">
        <span>{error}</span>
        <button class="icon-btn" aria-label={t("dismiss")} onclick={() => (error = "")}><Icon name="close" size={16} /></button>
      </div>
    {/if}

    {#if selected.size > 0}
      <div class="selbar" role="toolbar" aria-label="Selection actions">
        <span>{t("bulkSelected", { n: selected.size })}</span>
        <button class="btn btn-ghost" onclick={() => bulk(api.resume)}><Icon name="play" size={15} /> {t("resume")}</button>
        <button class="btn btn-ghost" onclick={() => bulk(api.pause)}><Icon name="pause" size={15} /> {t("pause")}</button>
        <button class="btn btn-ghost" onclick={() => bulk((id) => api.remove(id, false))}><Icon name="trash" size={15} /> {t("remove")}</button>
        <button class="btn btn-ghost" onclick={() => (selected = new Set())}>{t("close")}</button>
      </div>
    {/if}

    <div class="main-scroll">
      {#if filtered.length === 0}
        <div class="empty">
          <Icon name="inbox" size={56} />
          {#if all.length === 0}
            <h2>{t("emptyTitle")}</h2>
            <p>{t("emptySub")}</p>
            <div class="onboard">
              <button class="ob-card" onclick={() => addEl?.focus()}><Icon name="add" size={20} /><span>{t("obPaste")}</span></button>
              <button class="ob-card" onclick={() => (showMedia = true)}><Icon name="video" size={20} /><span>{t("obVideo")}</span></button>
              <button class="ob-card" onclick={() => (showGrabber = true)}><Icon name="link" size={20} /><span>{t("obLinks")}</span></button>
              <button class="ob-card" onclick={setupBrowser}><Icon name="download" size={20} /><span>{t("obBrowser")}</span></button>
            </div>
            <p class="keys"><kbd>Ctrl</kbd> <kbd>N</kbd> {t("emptyToAdd")} · <kbd>?</kbd> {t("emptyForShortcuts")}</p>
          {:else}
            <h2>{t("noMatchTitle")}</h2>
            <p>{t("noMatchSub")}</p>
          {/if}
        </div>
      {:else}
        <ul class="dl-list" role="list">
          {#each listItems as item, i (item.kind === "pkg" ? `p${item.pkg.id}` : `d${item.d.id}`)}
            {#if item.kind === "pkg"}
              <PackageGroup pkg={item.pkg} items={item.items} collapsed={collapsedPkgs.has(item.pkg.id)} ontoggle={togglePkg} onact={act} />
              {#if !collapsedPkgs.has(item.pkg.id)}
                {#each item.items as d, j (d.id)}
                  <DownloadRow {d} i={i + j} grouped onact={act} selected={selected.has(d.id)} onselect={toggleSelect} onmenu={openMenu} onreorder={reorder} expanded={expandedId === d.id} ondetails={toggleDetails} />
                  {#if expandedId === d.id}
                    <DetailPanel {d} onact={act} />
                  {/if}
                {/each}
              {/if}
            {:else}
              <DownloadRow d={item.d} {i} onact={act} selected={selected.has(item.d.id)} onselect={toggleSelect} onmenu={openMenu} onreorder={reorder} expanded={expandedId === item.d.id} ondetails={toggleDetails} />
              {#if expandedId === item.d.id}
                <DetailPanel d={item.d} onact={act} />
              {/if}
            {/if}
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
      <h2 id="help-h">{t("shortcutsTitle")}</h2>
      <button class="icon-btn" aria-label={t("close")} onclick={() => (showHelp = false)}><Icon name="close" size={18} /></button>
    </div>
    <div class="shortcuts">
      <span class="k"><kbd>Ctrl</kbd><kbd>N</kbd></span><span class="d">{t("scFocusAdd")}</span>
      <span class="k"><kbd>/</kbd></span><span class="d">{t("scSearch")}</span>
      <span class="k"><kbd>1</kbd>–<kbd>6</kbd></span><span class="d">{t("scFilter")}</span>
      <span class="k"><kbd>Ctrl</kbd><kbd>A</kbd></span><span class="d">{t("scSelectAll")}</span>
      <span class="k"><kbd>Ctrl</kbd><kbd>,</kbd></span><span class="d">{t("scSettings")}</span>
      <span class="k"><kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>P</kbd></span><span class="d">{t("scPauseAll")}</span>
      <span class="k"><kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>R</kbd></span><span class="d">{t("scResumeAll")}</span>
      <span class="k"><kbd>Space</kbd></span><span class="d">{t("scPauseResume")}</span>
      <span class="k"><kbd>Del</kbd></span><span class="d">{t("scRemove")}</span>
      <span class="k"><kbd>Enter</kbd></span><span class="d">{t("scOpen")}</span>
      <span class="k"><kbd>Esc</kbd></span><span class="d">{t("scClose")}</span>
    </div>
  </div>
{/if}

{#if scheduleFor}
  <div class="overlay" onclick={() => (scheduleFor = null)} role="presentation"></div>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="sched-h" tabindex="-1" use:trapFocus={{ onEscape: () => (scheduleFor = null) }}>
    <div class="dhead">
      <h2 id="sched-h">{t("scheduleAction")}</h2>
      <button class="icon-btn" aria-label={t("close")} onclick={() => (scheduleFor = null)}><Icon name="close" size={18} /></button>
    </div>
    <p class="hint" style="overflow:hidden; text-overflow:ellipsis; white-space:nowrap">{scheduleFor.filename || scheduleFor.url}</p>
    <div class="srow" style="margin-top:0.4rem">
      <span>{t("scheduleAt")}</span>
      <input type="datetime-local" bind:value={scheduleAt} aria-label={t("scheduleAt")} />
    </div>
    <button class="btn btn-primary" style="margin-top:0.8rem" onclick={saveScheduleAt}>
      <Icon name="clock" size={16} /> {t("scheduleAction")}
    </button>
  </div>
{/if}

{#if clipboardUrl}
  <div class="toast" role="status" aria-live="polite">
    <Icon name="download" size={18} />
    <span class="u">{clipboardUrl.length > 54 ? clipboardUrl.slice(0, 54) + "…" : clipboardUrl}</span>
    <button class="btn btn-primary" onclick={addClipboard}>{t("download")}</button>
    <button class="btn btn-ghost" onclick={() => (clipboardUrl = null)}>{t("dismiss")}</button>
  </div>
{/if}

{#if updateInfo}
  <div class="toast" role="status" aria-live="polite">
    <Icon name="download" size={18} />
    <span class="u">{t("updateAvailable", { v: updateInfo.latest })}</span>
    <button class="btn btn-primary" onclick={doInstall}>{updateInfo.can_install ? t("updateInstall") : t("updateView")}</button>
    <button class="btn btn-ghost" onclick={() => (updateInfo = null)}>{t("dismiss")}</button>
  </div>
{/if}

{#if menu}
  <div class="ctx-backdrop" onclick={() => (menu = null)} oncontextmenu={(e) => { e.preventDefault(); menu = null; }} role="presentation"></div>
  <div class="ctx-menu" style="left:{menu.x}px; top:{menu.y}px" role="menu">
    <button role="menuitem" onclick={() => { const id = menu!.d.id; menu = null; toggleDetails(id); }}>{t("detailTitle")}</button>
    <button role="menuitem" onclick={() => { navigator.clipboard.writeText(menu!.d.url).catch(() => {}); menu = null; }}>{t("copyUrl")}</button>
    {#if menu.d.status === "complete"}
      <button role="menuitem" onclick={() => { const id = menu!.d.id; menu = null; act(() => api.openFolder(id)); }}>{t("openFolder")}</button>
    {/if}
    {#if menu.d.status === "error"}
      <button role="menuitem" onclick={() => { const id = menu!.d.id; menu = null; act(() => api.retry(id)); }}>{t("retry")}</button>
    {/if}
    {#if menu.d.status === "scheduled"}
      <button role="menuitem" onclick={() => { const id = menu!.d.id; menu = null; act(() => api.scheduleDownload(id, null)); }}>{t("scheduleCancel")}</button>
    {:else if menu.d.status !== "complete"}
      <button role="menuitem" onclick={() => { const d = menu!.d; menu = null; openSchedule(d); }}>{t("scheduleAction")}</button>
    {/if}
    <button role="menuitem" onclick={() => { const id = menu!.d.id; menu = null; act(() => api.remove(id, false)); }}>{t("remove")}</button>
    <button role="menuitem" class="danger" onclick={() => { const id = menu!.d.id; menu = null; act(() => api.remove(id, true)); }}>{t("removeDelete")}</button>
  </div>
{/if}
