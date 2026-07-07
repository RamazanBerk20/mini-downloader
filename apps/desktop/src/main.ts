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
    // "system" (or unset) → keep following navigator.language from first paint.
    if (v && v !== "system") setLocale(normalizeLocale(v));
  })
  .catch(() => {});

// Apply the saved theme override (light/dark). Absent → CSS follows the OS.
api
  .getSetting("theme")
  .then((v) => {
    if (v === "light" || v === "dark") document.documentElement.dataset.theme = v;
  })
  .catch(() => {});

export default app;
