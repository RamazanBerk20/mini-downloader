<script lang="ts">
  import { api } from "./api";
  import { trapFocus } from "./lib/a11y";
  import Icon from "./lib/Icon.svelte";
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
  function toggle(u: string) {
    const s = new Set(checked);
    s.has(u) ? s.delete(u) : s.add(u);
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
<div class="modal" role="dialog" aria-modal="true" aria-labelledby="grab-h" tabindex="-1" use:trapFocus={{ onEscape: onclose }}>
  <div class="dhead">
    <h2 id="grab-h">Grab links</h2>
    <button class="icon-btn" aria-label="Close" onclick={onclose}><Icon name="close" size={18} /></button>
  </div>

  <textarea rows="6" placeholder="Paste text, a link list, or HTML" bind:value={text} aria-label="Text to extract links from"></textarea>
  <button class="btn" onclick={parse}><Icon name="search" size={16} /> Extract links</button>

  {#if error}<p class="hint" style="color:var(--error-fg)">{error}</p>{/if}

  {#if links.length}
    <p class="hint">{checked.size} of {links.length} selected</p>
    <div class="linklist">
      {#each links as l (l.url)}
        <label class="linkrow">
          <input type="checkbox" checked={checked.has(l.url)} onchange={() => toggle(l.url)} aria-label={l.url} />
          <span class="tag">{l.kind}</span>
          <span class="u">{l.url}</span>
        </label>
      {/each}
    </div>
    <button class="btn btn-primary" style="margin-top:0.8rem" onclick={addSelected}>
      <Icon name="download" size={16} /> Add {checked.size} selected
    </button>
  {/if}
</div>
