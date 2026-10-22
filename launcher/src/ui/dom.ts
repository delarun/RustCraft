import { describe, lang, t } from '../i18n';

export const $ = <T extends HTMLElement = HTMLElement>(id: string) => {
  const element = document.getElementById(id);
  if (!element) throw new Error(`#${id} missing`);
  return element as T;
};

/** Small per-browser preferences (selected version, toggles). */
export const prefs = {
  get(key: string): string | null {
    try {
      return localStorage.getItem(`mcweb.${key}`);
    } catch {
      return null;
    }
  },
  set(key: string, value: string) {
    try {
      localStorage.setItem(`mcweb.${key}`, value);
    } catch {
      /* storage unavailable */
    }
  },
};

export function formatBytes(bytes: number): string {
  const units = t.byteUnits.split(' ');
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = unit === 0 ? 0 : 1;
  return `${value.toLocaleString(lang, { minimumFractionDigits: digits, maximumFractionDigits: digits })} ${units[unit]}`;
}

export { describe };

export function setText(element: HTMLElement, message: string, isError = false) {
  element.textContent = message;
  element.classList.toggle('error', isError);
}
