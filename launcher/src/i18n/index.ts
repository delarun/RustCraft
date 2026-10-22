// The page's language: English or Russian, detected from the browser and
// switchable in the header (remembered in localStorage).

import { en, type MessageKey, type Messages } from './en';
import { errorText, format, type Arg, type Text } from './error';
import { ru } from './ru';

export type Lang = 'en' | 'ru';
export const LANGUAGES: Record<Lang, { name: string; messages: Messages }> = {
  en: { name: 'English', messages: en },
  ru: { name: 'Русский', messages: ru },
};

const PREF = 'mcweb.lang';

/** ru* → Russian, anything else → English. */
export function detectLang(languages: readonly string[]): Lang {
  const first = languages.find(Boolean)?.toLowerCase() ?? '';
  return first === 'ru' || first.startsWith('ru-') ? 'ru' : 'en';
}

function savedLang(): Lang | null {
  try {
    const value = localStorage.getItem(PREF);
    return value === 'en' || value === 'ru' ? value : null;
  } catch {
    return null;
  }
}

export let lang: Lang = 'en';
/** The current catalog. */
export let t: Messages = en;
const listeners: (() => void)[] = [];

export function setLang(next: Lang, remember = true) {
  lang = next;
  t = LANGUAGES[next].messages;
  if (remember) {
    try {
      localStorage.setItem(PREF, next);
    } catch {
      /* storage unavailable */
    }
  }
  if (typeof document !== 'undefined') {
    document.documentElement.lang = next;
    applyStatic(document);
  }
  for (const listener of listeners) listener();
}

export function initLang() {
  setLang(savedLang() ?? detectLang(typeof navigator === 'undefined' ? [] : navigator.languages), false);
}

export function onLangChange(listener: () => void) {
  listeners.push(listener);
}

export const tr = (key: MessageKey, ...args: Arg[]) => format(t, key, args);
export const translate = (text: Text) => format(t, text.key, text.args);
/** An error's message in the current language. */
export const describe = (error: unknown) => translate(errorText(error));

/**
 * Fills the page's static text: data-i18n (text), data-i18n-html (trusted
 * catalog markup), data-i18n-label (aria-label).
 */
export function applyStatic(root: ParentNode) {
  for (const element of root.querySelectorAll<HTMLElement>('[data-i18n]')) {
    element.textContent = tr(element.dataset.i18n as MessageKey);
  }
  for (const element of root.querySelectorAll<HTMLElement>('[data-i18n-html]')) {
    element.innerHTML = tr(element.dataset.i18nHtml as MessageKey);
  }
  for (const element of root.querySelectorAll<HTMLElement>('[data-i18n-label]')) {
    element.setAttribute('aria-label', tr(element.dataset.i18nLabel as MessageKey));
  }
}

export type { MessageKey, Messages, Text };
