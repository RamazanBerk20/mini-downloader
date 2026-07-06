import { mount } from "svelte";
import App from "./App.svelte";
import { api } from "./api";
import { setLocale, normalizeLocale } from "./lib/i18n.svelte";
import "./fonts.css";
import "./app.css";

// First paint follows the OS/browser language; corrected below once the saved
// preference loads. setLocale() is reactive, so a later change re-renders.
setLocale(normalizeLocale(navigator.language));

const app = mount(App, {
  target: document.getElementById("app")!,
});

api
  .getSetting("locale")
  .then((v) => {
    if (v) setLocale(normalizeLocale(v));
  })
  .catch(() => {});

export default app;
