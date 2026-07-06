<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "./api";
  import type { Category } from "./types";

  let { onclose }: { onclose: () => void } = $props();

  let autoOrganize = $state(true);
  let categories = $state<Category[]>([]);
  let browserStatus = $state("");

  onMount(async () => {
    autoOrganize = (await api.getSetting("auto_organize")) !== "false";
    categories = await api.listCategories();
  });

  async function toggleOrganize(e: Event) {
    autoOrganize = (e.target as HTMLInputElement).checked;
    await api.setSetting("auto_organize", autoOrganize ? "true" : "false");
  }

  async function saveDir(c: Category, e: Event) {
    c.dir = (e.target as HTMLInputElement).value;
    await api.saveCategory(c.name, c.dir, c.rules, c.priority);
  }

  async function installBrowser() {
    try {
      const p = await api.installBrowser();
      browserStatus = "Installed host manifest: " + p;
    } catch (e) {
      browserStatus = "Error: " + String(e);
    }
  }
</script>

<div class="overlay" onclick={onclose} role="presentation"></div>
<aside class="drawer">
  <div class="dhead">
    <h2>Settings</h2>
    <button onclick={onclose}>✕</button>
  </div>

  <section>
    <label class="srow">
      <span>Auto-organize finished files</span>
      <input type="checkbox" checked={autoOrganize} onchange={toggleOrganize} />
    </label>
    <p class="hint">Move completed single-file HTTP downloads into their category folder.</p>
  </section>

  <section>
    <h3>Firefox integration</h3>
    <button onclick={installBrowser}>Install native-messaging host</button>
    {#if browserStatus}<p class="hint">{browserStatus}</p>{/if}
    <p class="hint">Load the extension from <code>extension/</code> via <code>about:debugging</code>.</p>
  </section>

  <section>
    <h3>Categories</h3>
    {#each categories as c (c.id)}
      <div class="cat">
        <strong>{c.name}</strong>
        <input value={c.dir} onchange={(e) => saveDir(c, e)} />
      </div>
    {/each}
  </section>
</aside>
