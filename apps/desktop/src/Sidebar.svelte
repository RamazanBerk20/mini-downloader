<script lang="ts">
  import Icon from "./lib/Icon.svelte";
  import type { IconName } from "./lib/icons";
  import type { Category, Download } from "./types";
  import { t, type MsgKey } from "./lib/i18n.svelte";

  let {
    all,
    categories,
    statusFilter,
    categoryId,
    globalSpeed,
    onStatus,
    onCategory,
    onSpeed,
    onSettings,
  }: {
    all: Download[];
    categories: Category[];
    statusFilter: string;
    categoryId: number | null;
    globalSpeed: string;
    onStatus: (s: string) => void;
    onCategory: (id: number | null) => void;
    onSpeed: (v: string) => void;
    onSettings: () => void;
  } = $props();

  const STATUS: { key: string; label: MsgKey; icon: IconName }[] = [
    { key: "all", label: "statusAll", icon: "list" },
    { key: "active", label: "statusActive", icon: "download" },
    { key: "paused", label: "statusPaused", icon: "pause" },
    { key: "complete", label: "statusCompleted", icon: "check" },
    { key: "error", label: "statusFailed", icon: "warning" },
  ];
  const SPEEDS: [string, string][] = [
    ["0", ""],
    ["512000", "500 KB/s"],
    ["1048576", "1 MB/s"],
    ["5242880", "5 MB/s"],
    ["10485760", "10 MB/s"],
  ];

  const counts = $derived({
    all: all.length,
    active: all.filter((d) => d.status === "active" || d.status === "waiting").length,
    paused: all.filter((d) => d.status === "paused").length,
    complete: all.filter((d) => d.status === "complete").length,
    error: all.filter((d) => d.status === "error").length,
  } as Record<string, number>);

  function catCount(id: number): number {
    return all.filter((d) => d.category_id === id).length;
  }
</script>

<nav class="sidebar" aria-label="Filters">
  <h1 class="wordmark">Mini Downloader</h1>

  <div class="nav-group">
    <span class="nav-label" id="nav-status">{t("navStatus")}</span>
    {#each STATUS as s}
      <button
        class="nav-item"
        aria-current={statusFilter === s.key && categoryId === null ? "page" : undefined}
        onclick={() => onStatus(s.key)}
      >
        <Icon name={s.icon} size={16} />
        <span class="label">{t(s.label)}</span>
        <span class="count">{counts[s.key] ?? 0}</span>
      </button>
    {/each}
  </div>

  {#if categories.length}
    <div class="nav-group">
      <span class="nav-label">{t("navCategories")}</span>
      {#each categories as c (c.id)}
        <button
          class="nav-item"
          aria-current={categoryId === c.id ? "page" : undefined}
          onclick={() => onCategory(c.id)}
        >
          <Icon name="folder" size={16} />
          <span class="label">{c.name}</span>
          <span class="count">{catCount(c.id)}</span>
        </button>
      {/each}
    </div>
  {/if}

  <div class="side-foot">
    <label class="side-speed">
      <Icon name="gauge" size={16} />
      <span class="sr-only">{t("globalSpeed")}</span>
      <select value={globalSpeed} onchange={(e) => onSpeed((e.target as HTMLSelectElement).value)}>
        {#each SPEEDS as [v, label]}<option value={v}>{v === "0" ? t("speedUnlimited") : label}</option>{/each}
      </select>
    </label>
    <button class="nav-item" onclick={onSettings}>
      <Icon name="gear" size={16} />
      <span class="label">{t("navSettings")}</span>
    </button>
  </div>
</nav>
