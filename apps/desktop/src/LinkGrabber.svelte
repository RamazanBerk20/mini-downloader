<script lang="ts">
  import { api } from "./api";
  import type { ParsedLink } from "./types";

  let { onclose }: { onclose: () => void } = $props();

  let text = $state("");
  let links = $state<ParsedLink[]>([]);
  let checked = $state<Set<string>>(new Set());
  let error = $state("");

  async function parse() {
    error = "";
    try {
      links = await api.grabLinks(text);
      checked = new Set(links.map((l) => l.url));
    } catch (e) {
      error = String(e);
    }
  }

  function toggle(url: string) {
    const s = new Set(checked);
    if (s.has(url)) s.delete(url);
    else s.add(url);
    checked = s;
  }

  async function addSelected() {
    const urls = links.filter((l) => checked.has(l.url)).map((l) => l.url);
    if (!urls.length) return;
    try {
      await api.addLinksBatch(urls);
      onclose();
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<aside class="drawer">
  <div class="dhead"><h2>Grab links</h2><button onclick={onclose}>✕</button></div>

  <textarea rows="6" placeholder="Paste text, a link list, or HTML…" bind:value={text}></textarea>
  <button onclick={parse}>Extract links</button>

  {#if error}<p class="err">{error}</p>{/if}

  {#if links.length}
    <p class="hint">{checked.size} of {links.length} selected</p>
    <div class="linklist">
      {#each links as l (l.url)}
        <label class="cat">
          <input type="checkbox" checked={checked.has(l.url)} onchange={() => toggle(l.url)} />
          <span class="hint" style="flex:1; word-break:break-all">[{l.kind}] {l.url}</span>
        </label>
      {/each}
    </div>
    <button onclick={addSelected} style="margin-top:0.8rem">Add {checked.size} selected</button>
  {/if}
</aside>
