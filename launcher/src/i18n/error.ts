// Errors that carry a message key, so the page can show them in the player's
// language. Their `message` is the English text (logs, tests, workers).

import { en, type MessageKey, type Messages } from './en';

export type Arg = string | number | boolean;

/** A message key with its arguments: what workers post instead of text. */
export interface Text {
  key: MessageKey;
  args?: Arg[];
}

export function format(catalog: Messages, key: MessageKey, args: Arg[] = []): string {
  const message = catalog[key] as string | null | ((...args: Arg[]) => string);
  return typeof message === 'function' ? message(...args) : (message ?? '');
}

export class LocalizedError extends Error {
  constructor(
    readonly key: MessageKey,
    readonly args: Arg[] = [],
  ) {
    super(format(en, key, args));
  }

  get text(): Text {
    return { key: this.key, args: this.args };
  }
}

/** Any error as a Text (unknown errors keep their own message). */
export function errorText(error: unknown): Text {
  if (error instanceof LocalizedError) return error.text;
  return { key: 'text', args: [error instanceof Error ? error.message : String(error)] };
}
