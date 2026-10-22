// Requests to Microsoft, Xbox and Minecraft services, through the same-origin
// proxy where needed (see mojang/mirror.ts).

import { LocalizedError, type Arg } from '../i18n/error';
import type { MessageKey } from '../i18n/en';
import { mirrorUrl } from '../mojang/mirror';

/** A sign-in failure: a message key for the page, plus a code for the program. */
export class AuthError extends LocalizedError {
  constructor(
    key: MessageKey,
    args: Arg[],
    readonly code: string,
    readonly status = 0,
  ) {
    super(key, args);
  }
}

export interface Reply {
  status: number;
  body: Record<string, unknown>;
}

async function send(url: string, init: RequestInit, signal?: AbortSignal): Promise<Reply> {
  let response: Response;
  try {
    response = await fetch(mirrorUrl(url), { ...init, signal });
  } catch (error) {
    if (signal?.aborted) throw error;
    throw new AuthError('network', [new URL(url).host, error instanceof Error ? error.message : String(error)], 'network');
  }
  const text = await response.text();
  let body: Record<string, unknown> = {};
  if (text) {
    try {
      body = JSON.parse(text) as Record<string, unknown>;
    } catch {
      body = { text };
    }
  }
  return { status: response.status, body };
}

export function postForm(url: string, params: Record<string, string>, signal?: AbortSignal): Promise<Reply> {
  return send(url, { method: 'POST', headers: { 'Content-Type': 'application/x-www-form-urlencoded' }, body: new URLSearchParams(params) }, signal);
}

export function postJson(url: string, data: unknown, headers: Record<string, string> = {}): Promise<Reply> {
  return send(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json', ...headers },
    body: JSON.stringify(data),
  });
}

export function getJson(url: string, token: string): Promise<Reply> {
  return send(url, { headers: { Authorization: `Bearer ${token}`, Accept: 'application/json' } });
}

export const str = (value: unknown): string => (typeof value === 'string' ? value : '');
export const num = (value: unknown): number => (typeof value === 'number' ? value : Number(value) || 0);
