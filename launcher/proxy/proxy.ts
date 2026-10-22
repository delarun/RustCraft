// The launcher's same-origin proxy for hosts that send no usable CORS headers
// (see src/mojang/mirror.ts): /proxy/<host>/<path> → https://<host>/<path>,
// for an allowlist of Mojang, Microsoft and Xbox hosts only.
//
// - Nothing is stored: responses stream through; no caching on our side, and
//   `Cache-Control: no-store` keeps CDNs in front of us from caching either
//   (the browser keeps game files in OPFS).
// - Nothing sensitive is logged: one line per request with method, host,
//   status and duration — no paths, query strings, headers or bodies.
// - Only the request headers the services need are forwarded (so Origin,
//   Referer and Cookie never are: Microsoft's token endpoint refuses
//   cross-origin redemption for public-client apps); Set-Cookie is dropped.
//
// - Only our own pages may use it: a request whose Origin is another site, or
//   that the browser marks Sec-Fetch-Site: cross-site, is refused (the proxy
//   sends no CORS headers either, so other sites could not read answers anyway).
//
// Used by server.ts (production); vite.config.ts has the equivalent dev proxy.

import { request as httpRequest, type IncomingMessage, type OutgoingHttpHeaders, type ServerResponse } from 'node:http';
import { request as httpsRequest } from 'node:https';

export const PREFIX = '/proxy/';

/** Sign-in: Microsoft, Xbox Live, Minecraft services. */
export const AUTH_HOSTS = ['login.microsoftonline.com', 'user.auth.xboxlive.com', 'xsts.auth.xboxlive.com', 'api.minecraftservices.com'];
/** Mojang file hosts without CORS headers (piston-meta/piston-data are fetched directly). */
export const FILE_HOSTS = ['libraries.minecraft.net', 'resources.download.minecraft.net'];

const FORWARDED_REQUEST_HEADERS = [
  'accept',
  'accept-encoding',
  'authorization',
  'content-type',
  'content-length',
  'x-xbl-contract-version',
  'range',
  'if-range',
  'if-none-match',
  'if-modified-since',
];
const DROPPED_RESPONSE_HEADERS = new Set([
  'set-cookie',
  'set-cookie2',
  'connection',
  'keep-alive',
  'transfer-encoding',
  'upgrade',
  'trailer',
  'proxy-authenticate',
  'alt-svc',
  'strict-transport-security',
  'report-to',
  'nel',
  'cache-control',
  'expires',
  'age',
]);

/** Sign-in request bodies are a few kilobytes. */
export const MAX_REQUEST_BODY = 64 * 1024;

/** Production origin (see src/brand.ts). */
export const SITE_ORIGIN = 'https://rustcraft.online';

export interface ProxyOptions {
  /** Origins whose pages may use the proxy, besides the server's own (default: SITE_ORIGIN). */
  allowedOrigins?: string[];
  /** Where requests for a host go (tests point it elsewhere). */
  upstream?: (host: string) => { protocol: 'https:' | 'http:'; hostname: string; port: number };
  log?: (line: string) => void;
  timeoutMs?: number;
}

/** A handler for Node's http server (or a connect middleware): true when it took the request. */
export function createProxy(options: ProxyOptions = {}) {
  const upstream = options.upstream ?? ((host: string) => ({ protocol: 'https:' as const, hostname: host, port: 443 }));
  const log = options.log ?? ((line: string) => console.log(line));
  const timeoutMs = options.timeoutMs ?? 60_000;
  const allowedOrigins = new Set(options.allowedOrigins ?? [SITE_ORIGIN]);
  /** Same-origin requests (GET carries no Origin) or our allowed sites. */
  const fromOurPage = (req: IncomingMessage) => {
    if (req.headers['sec-fetch-site'] === 'cross-site') return false;
    const origin = req.headers.origin;
    if (!origin) return true;
    const host = req.headers.host;
    return allowedOrigins.has(origin) || (host !== undefined && (origin === `https://${host}` || origin === `http://${host}`));
  };

  return function proxy(req: IncomingMessage, res: ServerResponse, next?: () => void): boolean {
    const url = req.url ?? '/';
    if (!url.startsWith(PREFIX)) {
      next?.();
      return false;
    }
    const started = Date.now();
    const rest = url.slice(PREFIX.length);
    const slash = rest.indexOf('/');
    const host = slash < 0 ? rest : rest.slice(0, slash);
    const path = slash < 0 ? '/' : rest.slice(slash);
    const method = req.method ?? 'GET';
    const auth = AUTH_HOSTS.includes(host);
    const done = (status: number) => log(`proxy ${method} ${host || '-'} ${status} ${Date.now() - started}ms`);
    const fail = (status: number, message: string) => {
      res.writeHead(status, { 'Content-Type': 'text/plain; charset=utf-8', 'Cache-Control': 'no-store' });
      res.end(message);
      done(status);
    };

    if (!auth && !FILE_HOSTS.includes(host)) {
      fail(404, 'unknown host');
      return true;
    }
    if (!fromOurPage(req)) {
      fail(403, 'cross-site use of this proxy is not allowed');
      return true;
    }
    if (!(method === 'GET' || method === 'HEAD' || (auth && method === 'POST'))) {
      fail(405, 'method not allowed');
      return true;
    }
    const length = Number(req.headers['content-length'] ?? 0);
    if (method === 'POST' && length > MAX_REQUEST_BODY) {
      fail(413, 'request body too large');
      return true;
    }

    const headers: OutgoingHttpHeaders = { host };
    for (const name of FORWARDED_REQUEST_HEADERS) {
      const value = req.headers[name];
      if (value !== undefined) headers[name] = value;
    }
    const target = upstream(host);
    const send = target.protocol === 'https:' ? httpsRequest : httpRequest;
    const outgoing = send(
      { protocol: target.protocol, hostname: target.hostname, port: target.port, method, path, headers, timeout: timeoutMs },
      (incoming) => {
        const status = incoming.statusCode ?? 502;
        const responseHeaders: OutgoingHttpHeaders = {};
        for (const [name, value] of Object.entries(incoming.headers)) {
          if (value !== undefined && !DROPPED_RESPONSE_HEADERS.has(name) && !name.startsWith('access-control-')) responseHeaders[name] = value;
        }
        responseHeaders['cache-control'] = 'no-store';
        responseHeaders['cross-origin-resource-policy'] = 'same-origin';
        responseHeaders['x-content-type-options'] = 'nosniff';
        res.writeHead(status, responseHeaders);
        incoming.pipe(res);
        incoming.on('end', () => done(status));
        incoming.on('error', () => res.destroy());
      },
    );
    outgoing.on('timeout', () => outgoing.destroy(new Error('upstream timeout')));
    outgoing.on('error', () => {
      if (!res.headersSent) fail(502, 'upstream unavailable');
      else res.destroy();
    });
    res.on('close', () => outgoing.destroy());

    if (method === 'POST') {
      let received = 0;
      req.on('data', (chunk: Buffer) => {
        received += chunk.length;
        if (received > MAX_REQUEST_BODY) {
          outgoing.destroy();
          if (!res.headersSent) fail(413, 'request body too large');
          req.destroy();
        }
      });
      req.pipe(outgoing);
    } else {
      outgoing.end();
    }
    return true;
  };
}
