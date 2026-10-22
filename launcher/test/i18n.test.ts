import { describe, expect, it } from 'vitest';
import { en, type Messages } from '../src/i18n/en';
import { format, LocalizedError } from '../src/i18n/error';
import { detectLang } from '../src/i18n';
import { ru } from '../src/i18n/ru';
import html from '../index.html?raw';

const catalogs: Record<string, Messages> = { en, ru };

describe('message catalogs', () => {
  it('have the same keys', () => {
    expect(Object.keys(ru).sort()).toEqual(Object.keys(en).sort());
  });

  it('have no empty messages', () => {
    for (const [name, catalog] of Object.entries(catalogs)) {
      for (const [key, value] of Object.entries(catalog)) {
        if (value === null) continue; // optional (disclaimerLocal)
        if (typeof value === 'function') {
          const args = Array.from({ length: value.length }, (_, i) => (i % 2 ? 1 : 'x'));
          expect((value as (...a: unknown[]) => string)(...args), `${name}.${key}`).not.toBe('');
        } else {
          expect(value.trim(), `${name}.${key}`).not.toBe('');
        }
      }
    }
  });

  it('take the same arguments', () => {
    for (const key of Object.keys(en) as (keyof Messages)[]) {
      const a = en[key];
      const b = ru[key];
      if (a === null || b === null) continue;
      expect(typeof b, key).toBe(typeof a);
      if (typeof a === 'function') expect((b as (...x: unknown[]) => string).length, key).toBe(a.length);
    }
  });

  it('cover every key the page uses', () => {
    for (const [, key] of html.matchAll(/data-i18n(?:-html|-label)?="([A-Za-z]+)"/g)) {
      expect(key! in en, key).toBe(true);
    }
  });

  it('keep the legal disclaimer in English and translate it only alongside', () => {
    expect(en.disclaimerLocal).toBeNull();
    expect(ru.disclaimerLocal).toMatch(/MOJANG/);
  });
});

describe('language', () => {
  it('detects Russian from ru*, English otherwise', () => {
    expect(detectLang(['ru-RU', 'en-US'])).toBe('ru');
    expect(detectLang(['ru'])).toBe('ru');
    expect(detectLang(['en-GB', 'ru'])).toBe('en');
    expect(detectLang(['de-DE'])).toBe('en');
    expect(detectLang(['rus'])).toBe('en');
    expect(detectLang([])).toBe('en');
  });

  it('formats keys with arguments in either language', () => {
    expect(format(en, 'gameExited', [3])).toBe('The game exited (code 3).');
    expect(format(ru, 'gameExited', [3])).toBe('Игра завершилась (код 3).');
    const error = new LocalizedError('runtimeNoModules', ['java-runtime-epsilon']);
    expect(error.message).toBe('Java runtime java-runtime-epsilon has no lib/modules');
    expect(format(ru, error.key, error.args)).toBe('В Java-рантайме java-runtime-epsilon нет lib/modules');
  });
});
