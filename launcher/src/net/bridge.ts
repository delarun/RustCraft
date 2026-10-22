// The launcher's side of rustcraft-bridge (bridge/ at the repository root):
// the program on the player's computer that joins the game's connections
// (WebSockets from this page) to TCP. The VM side is src/jvm/net.ts.

import { DEFAULT_BRIDGE } from '../jvm/net';

export { DEFAULT_BRIDGE };

/** Release binaries for Windows, macOS and Linux (GitHub Actions, tags bridge-v*). */
export const BRIDGE_DOWNLOAD_URL = 'https://github.com/delarun/RustCraft/releases/latest';

/**
 * A bridge address as typed: ws:// or wss://, a host, an optional port, no
 * path, query or credentials. Returns it normalized (no trailing slash), or
 * null when it is not one.
 */
export function parseBridgeAddress(value: string): string | null {
  const text = value.trim();
  if (!/^wss?:\/\//i.test(text)) return null;
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return null;
  }
  if ((url.protocol !== 'ws:' && url.protocol !== 'wss:') || !url.hostname) return null;
  if (url.username || url.password || url.search || url.hash || (url.pathname !== '/' && url.pathname !== '')) return null;
  return `${url.protocol}//${url.host}`;
}

/** The bridge the game uses: the saved setting when valid, otherwise the default. */
export function bridgeAddress(saved: string | null): string {
  return (saved && parseBridgeAddress(saved)) || DEFAULT_BRIDGE;
}
