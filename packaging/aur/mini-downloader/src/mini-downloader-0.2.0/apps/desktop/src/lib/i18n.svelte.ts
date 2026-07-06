// Lightweight reactive i18n. No runtime dependency — a Svelte 5 rune store plus
// per-locale message maps. `t()` reads the current locale reactively, so any
// component that calls it re-renders when the language changes.
import { en } from "../locales/en";
import { tr } from "../locales/tr";
import { es } from "../locales/es";
import { fr } from "../locales/fr";
import { de } from "../locales/de";
import { ru } from "../locales/ru";
import { ar } from "../locales/ar";
import { zh } from "../locales/zh";
import { ja } from "../locales/ja";
import { ko } from "../locales/ko";

export type Messages = typeof en;
export type MsgKey = keyof Messages;

// Native names shown in the picker; `rtl` flips document direction.
export const LOCALES = {
  en: { name: "English", rtl: false, messages: en },
  tr: { name: "Türkçe", rtl: false, messages: tr },
  es: { name: "Español", rtl: false, messages: es },
  fr: { name: "Français", rtl: false, messages: fr },
  de: { name: "Deutsch", rtl: false, messages: de },
  ru: { name: "Русский", rtl: false, messages: ru },
  ar: { name: "العربية", rtl: true, messages: ar },
  zh: { name: "中文", rtl: false, messages: zh },
  ja: { name: "日本語", rtl: false, messages: ja },
  ko: { name: "한국어", rtl: false, messages: ko },
} as const;

export type LocaleCode = keyof typeof LOCALES;

const state = $state({ code: "en" as LocaleCode });

export function currentLocale(): LocaleCode {
  return state.code;
}

/** Best-effort map of a BCP-47-ish tag (e.g. "tr-TR") to a supported locale. */
export function normalizeLocale(raw: string | null | undefined): LocaleCode {
  const base = (raw ?? "").toLowerCase().split(/[-_]/)[0];
  return (base in LOCALES ? base : "en") as LocaleCode;
}

export function setLocale(code: LocaleCode) {
  if (!(code in LOCALES)) code = "en";
  state.code = code;
  if (typeof document !== "undefined") {
    document.documentElement.lang = code;
    document.documentElement.dir = LOCALES[code].rtl ? "rtl" : "ltr";
  }
}

/** Translate `key`, interpolating `{name}` params. Falls back to English then the key. */
export function t(key: MsgKey, params?: Record<string, string | number>): string {
  const table = LOCALES[state.code].messages as Record<string, string>;
  let s = table[key] ?? (en as Record<string, string>)[key] ?? key;
  if (params) {
    for (const [k, v] of Object.entries(params)) s = s.split(`{${k}}`).join(String(v));
  }
  return s;
}
