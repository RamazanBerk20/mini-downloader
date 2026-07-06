// Accessibility helpers: focus trap for dialogs + a polite live-region announcer.

const FOCUSABLE =
  'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

/**
 * Svelte action: trap Tab focus inside `node`, close on Escape, and restore
 * focus to the previously focused element on teardown.
 */
export function trapFocus(node: HTMLElement, params: { onEscape?: () => void } = {}) {
  const previouslyFocused = document.activeElement as HTMLElement | null;

  const focusables = () =>
    Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.offsetParent !== null,
    );

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      params.onEscape?.();
      return;
    }
    if (e.key !== "Tab") return;
    const f = focusables();
    if (f.length === 0) return;
    const first = f[0];
    const last = f[f.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  // Focus the first control (or the container).
  (focusables()[0] ?? node).focus();
  node.addEventListener("keydown", onKey);

  return {
    update(p: { onEscape?: () => void }) {
      params = p;
    },
    destroy() {
      node.removeEventListener("keydown", onKey);
      previouslyFocused?.focus?.();
    },
  };
}

let region: HTMLElement | null = null;

/** Announce a message to screen readers via a shared polite live region. */
export function announce(message: string) {
  if (!region) {
    region = document.createElement("div");
    region.setAttribute("aria-live", "polite");
    region.setAttribute("aria-atomic", "true");
    region.className = "sr-only";
    document.body.appendChild(region);
  }
  region.textContent = "";
  // A tick later so repeat messages re-announce.
  setTimeout(() => {
    if (region) region.textContent = message;
  }, 40);
}
