// Hosts that send no usable CORS headers are reached through a same-origin
// proxy: https://<host>/<path> → /proxy/<host>/<path>. In development this is
// the Vite dev server (see vite.config.ts); a production deployment needs an
// equivalent reverse proxy limited to these hosts.
//
// Checked with curl and an Origin header (2026-10):
// - piston-meta.mojang.com, launchermeta.mojang.com, piston-data.mojang.com:
//   `Access-Control-Allow-Origin: *` — fetched directly.
// - libraries.minecraft.net, resources.download.minecraft.net: no CORS headers.
// - login.microsoftonline.com: /devicecode has no CORS headers; /token answers
//   preflights, but redeems tokens cross-origin only for SPA registrations
//   (AADSTS9002326), so the proxy also drops the Origin header.
// - api.minecraftservices.com: preflight answered with 405.
// - user.auth.xboxlive.com, xsts.auth.xboxlive.com: CORS on success, but
//   XSTS error responses (the ones that explain a missing Xbox profile) carry
//   none — proxied so the browser can read them.

export const PROXY_PREFIX = '/proxy';

export const PROXIED_HOSTS: readonly string[] = [
  'libraries.minecraft.net',
  'resources.download.minecraft.net',
  'login.microsoftonline.com',
  'user.auth.xboxlive.com',
  'xsts.auth.xboxlive.com',
  'api.minecraftservices.com',
];

export function mirrorUrl(url: string): string {
  const parsed = new URL(url);
  return PROXIED_HOSTS.includes(parsed.host) ? `${PROXY_PREFIX}/${parsed.host}${parsed.pathname}${parsed.search}` : url;
}

export const VERSION_MANIFEST_URL = 'https://piston-meta.mojang.com/mc/game/version_manifest_v2.json';
export const JAVA_RUNTIMES_URL =
  'https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json';
export const RESOURCES_URL = 'https://resources.download.minecraft.net';
