import { mount } from "svelte";
import App from "./App.svelte";
import { api } from "./api";
import { setLocale, normalizeLocale } from "./lib/i18n.svelte";
import "./fonts.css";
import "./app.css";

// First paint follows the WebView locale; corrected below with the native OS
// locale. Some Linux WebViews report English even in a Turkish desktop session.
// setLocale() is reactive, so a later change re-renders.
setLocale(normalizeLocale(navigator.language));

// Apply the last-known theme override synchronously (localStorage mirror of the
// DB setting) so the first paint doesn't flash the OS theme.
try {
  const cached = localStorage.getItem("theme");
  if (cached === "light" || cached === "dark") document.documentElement.dataset.theme = cached;
} catch {}

const app = mount(App, {
  target: document.getElementById("app")!,
});

async function applyLocalePreference() {
  const saved = await api.getSetting("locale").catch(() => null);
  if (saved && saved !== "system") {
    setLocale(normalizeLocale(saved));
    return;
  }

  const systemLocale = await api.getSystemLocale().catch(() => null);
  setLocale(normalizeLocale(systemLocale ?? navigator.language));
}

void applyLocalePreference();

// Apply the saved theme override (light/dark). Absent → CSS follows the OS.
api
  .getSetting("theme")
  .then((v) => {
    if (v === "light" || v === "dark") document.documentElement.dataset.theme = v;
    else delete document.documentElement.dataset.theme;
    try {
      localStorage.setItem("theme", v || "system");
    } catch {}
  })
  .catch(() => {});

export default app;
