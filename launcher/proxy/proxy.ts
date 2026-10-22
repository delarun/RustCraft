// The launcher's same-origin proxy for hosts that send no usable CORS headers
// (see src/mojang/mirror.ts): /proxy/<host>/<path> → https://<host>/<path>.
// It must not become a way to send traffic to Microsoft or Mojang under our
// name, so it forwards only what the launcher itself calls:
//
// - Routes: per host, the exact methods and paths the launcher uses (anchored
//   patterns, see ROUTES); no query strings, no `%`-escapes, `..`, `//` or
//   backslashes. Anything else gets 403 (or 405 for a known path).
// - Callers: only our own pages — a request whose Origin is another site, or
//   that the browser marks Sec-Fetch-Site: cross-site, gets 403 (and the proxy
//   sends no CORS headers, so other sites could not read answers anyway).
// - Rate limits per client IP: token buckets (tight for sign-in, looser for
//   game files), concurrent upstream requests per IP and in total; 429 with
//   Retry-After. Behind a reverse proxy the client IP is read from
//   CF-Connecting-IP / X-Forwarded-For only when `trustProxy` is set.
// - Upstream: redirects are not followed (a 3xx becomes 502), timeouts
//   (sign-in: 15 s in total; files: 15 s to the first byte, 30 s idle,
//   10 min in total), sign-in answers capped at 1 MiB, request bodies at 64 KiB.
// - Nothing is stored: responses stream through, `Cache-Control: no-store`
//   keeps CDNs in front of us from caching (the browser keeps game files in OPFS).
// - Nothing sensitive is logged: one line per request with method, host,
//   status and duration — no paths, query strings, headers, bodies or tokens.
// - Only the request headers the services need are forwarded (so Origin,
//   Referer and Cookie never are: Microsoft's token endpoint refuses
//   cross-origin redemption for public-client apps); Set-Cookie is dropped.
//
// Used by server.ts (production) and vite.config.ts (development).

import { request as httpRequest, type IncomingMessage, type OutgoingHttpHeaders, type ServerResponse } from 'node:http';
import { request as httpsRequest } from 'node:https';

export const PREFIX = '/proxy/';

/** Production origin (see src/brand.ts). */
export const SITE_ORIGIN = 'https://rustcraft.online';

type Kind = 'auth' | 'files';
interface Route {
  methods: string[];
  path: RegExp;
}

/** Maven artifact paths: group/…/artifact/version/file.jar, segments not starting with a dot. */
const MAVEN_JAR = /^\/(?:[A-Za-z0-9_][A-Za-z0-9_.+-]*\/){3,}[A-Za-z0-9_][A-Za-z0-9_.+-]*\.jar$/;

/** What the launcher calls (src/auth/*, src/download/*, src/jvm/opfs.ts), and nothing more. */
export const ROUTES: Record<string, { kind: Kind; routes: Route[] }> = {
  'login.microsoftonline.com': { kind: 'auth', routes: [{ methods: ['POST'], path: /^\/consumers\/oauth2\/v2\.0\/(?:devicecode|token)$/ }] },
  'user.auth.xboxlive.com': { kind: 'auth', routes: [{ methods: ['POST'], path: /^\/user\/authenticate$/ }] },
  'xsts.auth.xboxlive.com': { kind: 'auth', routes: [{ methods: ['POST'], path: /^\/xsts\/authorize$/ }] },
  'api.minecraftservices.com': {
    kind: 'auth',
    routes: [
      { methods: ['POST'], path: /^\/authentication\/login_with_xbox$/ },
      { methods: ['GET'], path: /^\/entitlements\/mcstore$/ },
      { methods: ['GET'], path: /^\/minecraft\/profile$/ },
    ],
  },
  'libraries.minecraft.net': { kind: 'files', routes: [{ methods: ['GET', 'HEAD'], path: MAVEN_JAR }] },
  'resources.download.minecraft.net': { kind: 'files', routes: [{ methods: ['GET', 'HEAD'], path: /^\/([0-9a-f]{2})\/\1[0-9a-f]{38}$/ }] },
};
export const AUTH_HOSTS = Object.keys(ROUTES).filter((host) => ROUTES[host]!.kind === 'auth');
export const FILE_HOSTS = Object.keys(ROUTES).filter((host) => ROUTES[host]!.kind === 'files');

/** 403 / 405 / null (allowed) for a request to an allowlisted host. */
export function checkRoute(host: string, method: string, path: string): 403 | 405 | null {
  const entry = ROUTES[host];
  if (!entry) return 403;
  // Raw path tricks: escapes (%2f, %2e…), backslashes, empty or dot segments, queries, fragments.
  if (/[%\\?#]/.test(path) || path.includes('//') || /(^|\/)\.\.?(\/|$)/.test(path)) return 403;
  const route = entry.routes.find((r) => r.path.test(path));
  if (!route) return 403;
  return route.methods.includes(method) ? null : 405;
}

export interface Limits {
  /** Bucket size (burst) and refill per minute. */
  burst: number;
  perMinute: number;
}

/**
 * Sign-in: a device code sign-in polls the token endpoint every 5 s (12/min)
 * and then makes five calls (Xbox, XSTS, Minecraft login, entitlements,
 * profile); a renewal makes six. 30/min with a burst of 15 covers both with
 * room for a retry, and stops anyone from using us as a sign-in relay.
 *
 * Files: a first launch fetches ~70 libraries and ~40 assets; sounds and
 * translations (~5000 objects) are fetched on demand. A full download of all
 * ~5000 objects (what other launchers do) finishes in about 4 minutes at
 * 20/s after a burst of 300 — enough for the game, and it caps one IP at a
 * level Mojang's CDN does not notice.
 */
export const DEFAULT_LIMITS: Record<Kind, Limits> = {
  auth: { burst: 15, perMinute: 30 },
  files: { burst: 300, perMinute: 1200 },
};

export interface ProxyOptions {
  /** Origins whose pages may use the proxy, besides the server's own (default: SITE_ORIGIN). */
  allowedOrigins?: string[];
  /** Where requests for a host go (tests point it elsewhere). */
  upstream?: (host: string) => { protocol: 'https:' | 'http:'; hostname: string; port: number };
  log?: (line: string) => void;
  /** Read the client IP from CF-Connecting-IP / X-Forwarded-For (only behind our own reverse proxy). */
  trustProxy?: boolean;
  limits?: Partial<Record<Kind, Limits>>;
  /** Concurrent upstream requests per client IP and in total. */
  maxConcurrentPerClient?: number;
  maxConcurrent?: number;
  timeouts?: Partial<Timeouts>;
  /** Largest sign-in response passed on. */
  maxAuthResponse?: number;
  /** Clock (tests). */
  now?: () => number;
}

interface Timeouts {
  /** Sign-in requests, in total. */
  auth: number;
  /** File requests: until the response starts, between chunks, in total. */
  filesFirstByte: number;
  filesIdle: number;
  filesTotal: number;
}
const DEFAULT_TIMEOUTS: Timeouts = { auth: 15_000, filesFirstByte: 15_000, filesIdle: 30_000, filesTotal: 600_000 };

/** Sign-in request bodies are a few kilobytes. */
export const MAX_REQUEST_BODY = 64 * 1024;
export const MAX_AUTH_RESPONSE = 1024 * 1024;

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
  'location',
]);

interface Bucket {
  tokens: number;
  updated: number;
}

/** Token buckets per (client, kind), with idle buckets evicted so memory stays bounded. */
export class RateLimiter {
  private buckets = new Map<string, Bucket>();
  private lastSweep = 0;
  private static MAX_BUCKETS = 100_000;
  private static SWEEP_MS = 60_000;

  private limits: Record<Kind, Limits>;
  private now: () => number;

  constructor(limits: Record<Kind, Limits>, now: () => number) {
    this.limits = limits;
    this.now = now;
  }

  /** 0 when allowed (a token is taken), otherwise the seconds until one is available. */
  take(client: string, kind: Kind): number {
    const { burst, perMinute } = this.limits[kind];
    const rate = perMinute / 60_000;
    const key = `${kind} ${client}`;
    const now = this.now();
    let bucket = this.buckets.get(key);
    if (bucket) {
      bucket.tokens = Math.min(burst, bucket.tokens + (now - bucket.updated) * rate);
      bucket.updated = now;
      // Most recently used last, for eviction.
      this.buckets.delete(key);
    } else {
      if (now - this.lastSweep >= RateLimiter.SWEEP_MS || this.buckets.size >= RateLimiter.MAX_BUCKETS) this.evict(now);
      bucket = { tokens: burst, updated: now };
    }
    this.buckets.set(key, bucket);
    if (bucket.tokens >= 1) {
      bucket.tokens -= 1;
      return 0;
    }
    return Math.max(1, Math.ceil((1 - bucket.tokens) / rate / 1000));
  }

  /** Drops buckets that have refilled completely (they hold no state), and the oldest beyond the cap. */
  evict(now = this.now()) {
    this.lastSweep = now;
    for (const [key, bucket] of this.buckets) {
      const { burst, perMinute } = this.limits[key.startsWith('auth ') ? 'auth' : 'files'];
      if (bucket.tokens + (now - bucket.updated) * (perMinute / 60_000) >= burst) this.buckets.delete(key);
    }
    while (this.buckets.size >= RateLimiter.MAX_BUCKETS) this.buckets.delete(this.buckets.keys().next().value!);
  }

  get size() {
    return this.buckets.size;
  }
}

export function clientAddress(req: IncomingMessage, trustProxy: boolean): string {
  if (trustProxy) {
    const cf = req.headers['cf-connecting-ip'];
    if (typeof cf === 'string' && cf.trim()) return cf.trim();
    const forwarded = req.headers['x-forwarded-for'];
    // The last entry is the one our reverse proxy added; earlier ones are the client's word.
    const last = (Array.isArray(forwarded) ? forwarded.join(',') : forwarded)?.split(',').map((s) => s.trim()).filter(Boolean).at(-1);
    if (last) return last;
  }
  return req.socket.remoteAddress ?? 'unknown';
}

/** A handler for Node's http server (or a connect middleware): true when it took the request. */
export function createProxy(options: ProxyOptions = {}) {
  const upstream = options.upstream ?? ((host: string) => ({ protocol: 'https:' as const, hostname: host, port: 443 }));
  const log = options.log ?? ((line: string) => console.log(line));
  const now = options.now ?? Date.now;
  const timeouts = { ...DEFAULT_TIMEOUTS, ...options.timeouts };
  const maxAuthResponse = options.maxAuthResponse ?? MAX_AUTH_RESPONSE;
  const maxPerClient = options.maxConcurrentPerClient ?? 16;
  const maxTotal = options.maxConcurrent ?? 256;
  const limiter = new RateLimiter({ ...DEFAULT_LIMITS, ...options.limits }, now);
  const active = new Map<string, number>();
  let activeTotal = 0;
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
    const started = now();
    const rest = url.slice(PREFIX.length);
    const slash = rest.indexOf('/');
    const host = slash < 0 ? rest : rest.slice(0, slash);
    const path = slash < 0 ? '/' : rest.slice(slash);
    const method = req.method ?? 'GET';
    const entry = Object.hasOwn(ROUTES, host) ? ROUTES[host]! : null;
    let logged = false;
    const done = (status: number) => {
      if (logged) return;
      logged = true;
      log(`proxy ${method} ${entry ? host : '-'} ${status} ${now() - started}ms`);
    };
    /** Set once the response is complete (or abandoned): later failures change nothing. */
    let answered = false;
    const fail = (status: number, message: string, headers: OutgoingHttpHeaders = {}) => {
      if (answered) return;
      answered = true;
      if (res.headersSent) {
        res.destroy();
        done(status);
        return;
      }
      res.writeHead(status, { 'Content-Type': 'text/plain; charset=utf-8', 'Cache-Control': 'no-store', ...headers });
      res.end(message);
      done(status);
    };

    if (!entry) {
      fail(404, 'unknown host');
      return true;
    }
    if (!fromOurPage(req)) {
      fail(403, 'cross-site use of this proxy is not allowed');
      return true;
    }
    const routeError = checkRoute(host, method, path);
    if (routeError) {
      fail(routeError, routeError === 405 ? 'method not allowed' : 'not an allowed path');
      return true;
    }
    if (method === 'POST' && Number(req.headers['content-length'] ?? 0) > MAX_REQUEST_BODY) {
      fail(413, 'request body too large');
      return true;
    }
    const client = clientAddress(req, options.trustProxy ?? false);
    const wait = limiter.take(client, entry.kind);
    if (wait) {
      fail(429, 'too many requests', { 'Retry-After': String(wait) });
      return true;
    }
    if ((active.get(client) ?? 0) >= maxPerClient || activeTotal >= maxTotal) {
      fail(429, 'too many concurrent requests', { 'Retry-After': '1' });
      return true;
    }
    active.set(client, (active.get(client) ?? 0) + 1);
    activeTotal++;
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      activeTotal--;
      const count = (active.get(client) ?? 1) - 1;
      if (count > 0) active.set(client, count);
      else active.delete(client);
    };
    res.on('close', release);

    const headers: OutgoingHttpHeaders = { host };
    for (const name of FORWARDED_REQUEST_HEADERS) {
      const value = req.headers[name];
      if (value !== undefined) headers[name] = value;
    }
    const auth = entry.kind === 'auth';
    const target = upstream(host);
    const send = target.protocol === 'https:' ? httpsRequest : httpRequest;
    // Node's http(s).request never follows redirects; 3xx answers are refused below.
    const outgoing = send({
      protocol: target.protocol,
      hostname: target.hostname,
      port: target.port,
      method,
      path,
      headers,
      timeout: auth ? timeouts.auth : timeouts.filesIdle,
    });
    const abort = (status: number, message: string) => {
      fail(status, message);
      outgoing.destroy();
    };
    const total = setTimeout(() => abort(504, 'upstream timeout'), auth ? timeouts.auth : timeouts.filesTotal);
    const firstByte = auth ? null : setTimeout(() => abort(504, 'upstream timeout'), timeouts.filesFirstByte);
    const clearTimers = () => {
      clearTimeout(total);
      if (firstByte) clearTimeout(firstByte);
    };
    res.on('close', () => {
      clearTimers();
      outgoing.destroy();
    });
    outgoing.on('timeout', () => abort(504, 'upstream timeout'));
    outgoing.on('error', () => abort(502, 'upstream unavailable'));
    outgoing.on('response', (incoming) => {
      if (firstByte) clearTimeout(firstByte);
      const status = incoming.statusCode ?? 502;
      if (status >= 300 && status < 400 && status !== 304) {
        incoming.resume();
        abort(502, 'upstream redirect not followed');
        return;
      }
      const length = Number(incoming.headers['content-length'] ?? 0);
      if (auth && length > maxAuthResponse) {
        incoming.resume();
        abort(502, 'upstream response too large');
        return;
      }
      const responseHeaders: OutgoingHttpHeaders = {};
      for (const [name, value] of Object.entries(incoming.headers)) {
        if (value !== undefined && !DROPPED_RESPONSE_HEADERS.has(name) && !name.startsWith('access-control-')) responseHeaders[name] = value;
      }
      responseHeaders['cache-control'] = 'no-store';
      responseHeaders['cross-origin-resource-policy'] = 'same-origin';
      responseHeaders['x-content-type-options'] = 'nosniff';
      if (auth) {
        // Sign-in answers are small: buffered whole, so an oversized one becomes a clean 502.
        const chunks: Buffer[] = [];
        let received = 0;
        incoming.on('data', (chunk: Buffer) => {
          received += chunk.length;
          if (received > maxAuthResponse) {
            incoming.destroy();
            abort(502, 'upstream response too large');
            return;
          }
          chunks.push(chunk);
        });
        incoming.on('end', () => {
          clearTimers();
          if (answered) return;
          answered = true;
          const body = Buffer.concat(chunks);
          delete responseHeaders['transfer-encoding'];
          responseHeaders['content-length'] = String(body.length);
          res.writeHead(status, responseHeaders);
          res.end(body);
          done(status);
        });
      } else {
        // Files stream through.
        res.writeHead(status, responseHeaders);
        incoming.on('data', (chunk: Buffer) => {
          if (!res.write(chunk)) {
            incoming.pause();
            res.once('drain', () => incoming.resume());
          }
        });
        incoming.on('end', () => {
          clearTimers();
          if (answered) return;
          answered = true;
          res.end();
          done(status);
        });
      }
      incoming.on('error', () => abort(502, 'upstream failed'));
    });

    if (method === 'POST') {
      let received = 0;
      req.on('data', (chunk: Buffer) => {
        received += chunk.length;
        if (received > MAX_REQUEST_BODY) {
          req.destroy();
          abort(413, 'request body too large');
        }
      });
      req.pipe(outgoing);
    } else {
      outgoing.end();
    }
    return true;
  };
}
