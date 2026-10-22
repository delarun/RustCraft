import { createServer, request, type IncomingHttpHeaders, type IncomingMessage, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { SITE_URL } from '../src/brand.ts';
import { checkRoute, clientAddress, createProxy, MAX_REQUEST_BODY, RateLimiter, SITE_ORIGIN, type ProxyOptions } from './proxy.ts';

type Mode = 'ok' | 'redirect' | 'stall' | 'big';
let mode: Mode = 'ok';
let upstream: Server;
let seen: { method?: string; url?: string; headers: IncomingHttpHeaders; body: string }[] = [];
const servers: Server[] = [];
const port = (server: Server) => (server.address() as AddressInfo).port;

const TOKEN = '/proxy/login.microsoftonline.com/consumers/oauth2/v2.0/token';
const JAR = '/proxy/libraries.minecraft.net/com/google/code/gson/gson/2.14.0/gson-2.14.0.jar';
const OBJECT = '/proxy/resources.download.minecraft.net/af/af96f55a90eaf11b327f1b5f8834a051027dc506';
const PROFILE = '/proxy/api.minecraftservices.com/minecraft/profile';

/** A front server running a proxy with these options against the fake upstream. */
async function front(options: ProxyOptions = {}): Promise<{ server: Server; logs: string[] }> {
  const logs: string[] = [];
  const proxy = createProxy({
    upstream: () => ({ protocol: 'http:', hostname: '127.0.0.1', port: port(upstream) }),
    log: (line) => logs.push(line),
    ...options,
  });
  const server = createServer((req, res) => {
    if (!proxy(req, res)) res.writeHead(404).end();
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  servers.push(server);
  return { server, logs };
}

function call(server: Server, path: string, init: { method?: string; headers?: Record<string, string>; body?: string } = {}) {
  return new Promise<{ status: number; headers: IncomingHttpHeaders; body: string }>((resolve, reject) => {
    const req = request({ host: '127.0.0.1', port: port(server), path, method: init.method ?? 'GET', headers: init.headers }, (res) => {
      let body = '';
      res.on('data', (c: Buffer) => (body += c));
      res.on('end', () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body }));
      res.on('error', () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body }));
    });
    req.on('error', reject);
    req.end(init.body);
  });
}

let main: { server: Server; logs: string[] };

beforeAll(async () => {
  upstream = createServer((req, res) => {
    let body = '';
    req.on('data', (c: Buffer) => (body += c));
    req.on('end', () => {
      seen.push({ method: req.method, url: req.url, headers: req.headers, body });
      if (mode === 'stall') return; // never answers
      if (mode === 'redirect') {
        res.writeHead(302, { Location: 'https://evil.example/' }).end();
        return;
      }
      res.writeHead(200, {
        'Content-Type': 'application/json',
        'Set-Cookie': 'session=1',
        'Cache-Control': 'public, max-age=86400',
        'Access-Control-Allow-Origin': '*',
      });
      res.end(mode === 'big' ? 'x'.repeat(4096) : JSON.stringify({ ok: true, size: body.length }));
    });
  });
  await new Promise<void>((r) => upstream.listen(0, '127.0.0.1', r));
  main = await front();
});

beforeEach(() => {
  mode = 'ok';
  seen = [];
});

afterAll(() => {
  for (const server of servers) server.close();
  upstream.closeAllConnections();
  upstream.close();
});

describe('routes', () => {
  it('allow exactly what the launcher calls', () => {
    const allowed: [string, string, string][] = [
      ['login.microsoftonline.com', 'POST', '/consumers/oauth2/v2.0/devicecode'],
      ['login.microsoftonline.com', 'POST', '/consumers/oauth2/v2.0/token'],
      ['user.auth.xboxlive.com', 'POST', '/user/authenticate'],
      ['xsts.auth.xboxlive.com', 'POST', '/xsts/authorize'],
      ['api.minecraftservices.com', 'POST', '/authentication/login_with_xbox'],
      ['api.minecraftservices.com', 'GET', '/entitlements/mcstore'],
      ['api.minecraftservices.com', 'GET', '/minecraft/profile'],
      ['libraries.minecraft.net', 'GET', '/org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar'],
      ['libraries.minecraft.net', 'HEAD', '/io/netty/netty-common/4.2.16.Final/netty-common-4.2.16.Final.jar'],
      ['resources.download.minecraft.net', 'GET', '/af/af96f55a90eaf11b327f1b5f8834a051027dc506'],
    ];
    for (const [host, method, path] of allowed) expect(checkRoute(host, method, path), `${method} ${host}${path}`).toBeNull();
  });

  it('refuse other paths, methods and path tricks', () => {
    const blocked: [string, string, string, number][] = [
      ['login.microsoftonline.com', 'POST', '/common/oauth2/v2.0/token', 403],
      ['login.microsoftonline.com', 'POST', '/consumers/oauth2/v2.0/authorize', 403],
      ['login.microsoftonline.com', 'POST', '/consumers/oauth2/v2.0/token/', 403],
      ['login.microsoftonline.com', 'POST', '/consumers/oauth2/v2.0/token?x=1', 403],
      ['login.microsoftonline.com', 'GET', '/consumers/oauth2/v2.0/token', 405],
      ['user.auth.xboxlive.com', 'POST', '/user/authenticate/../../admin', 403],
      ['api.minecraftservices.com', 'GET', '/minecraft/profile/skins', 403],
      ['api.minecraftservices.com', 'POST', '/minecraft/profile', 405],
      ['api.minecraftservices.com', 'GET', '/entitlements/license', 403],
      ['api.minecraftservices.com', 'GET', '/minecraft%2fprofile', 403],
      ['libraries.minecraft.net', 'GET', '/com/google/gson.jar', 403],
      ['libraries.minecraft.net', 'GET', '/org/lwjgl/../../etc/passwd/x.jar', 403],
      ['libraries.minecraft.net', 'GET', '/org/lwjgl/%2e%2e/lwjgl/3.4.3/lwjgl.jar', 403],
      ['libraries.minecraft.net', 'GET', '/org//lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar', 403],
      ['libraries.minecraft.net', 'GET', '/org\\lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar', 403],
      ['libraries.minecraft.net', 'GET', '/org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.pom', 403],
      ['libraries.minecraft.net', 'GET', '/org/lwjgl/lwjgl/3.4.3/.hidden.jar', 403],
      ['libraries.minecraft.net', 'GET', '/org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar?download', 403],
      ['libraries.minecraft.net', 'POST', '/org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar', 405],
      ['resources.download.minecraft.net', 'GET', '/ab/af96f55a90eaf11b327f1b5f8834a051027dc506', 403],
      ['resources.download.minecraft.net', 'GET', '/AF/AF96F55A90EAF11B327F1B5F8834A051027DC506', 403],
      ['resources.download.minecraft.net', 'GET', '/af/af96f55a90eaf11b327f1b5f8834a051027dc50', 403],
      ['resources.download.minecraft.net', 'GET', '/', 403],
      ['example.com', 'GET', '/', 403],
    ];
    for (const [host, method, path, status] of blocked) expect(checkRoute(host, method, path), `${method} ${host}${path}`).toBe(status);
  });

  it('answers 403/404/405 over HTTP without contacting upstream', async () => {
    expect((await call(main.server, '/proxy/example.com/')).status).toBe(404);
    expect((await call(main.server, '/proxy/api.minecraftservices.com/minecraft/profile/name/x', { method: 'GET' })).status).toBe(403);
    expect((await call(main.server, '/proxy/api.minecraftservices.com/x', { method: 'DELETE' })).status).toBe(403);
    expect((await call(main.server, PROFILE, { method: 'DELETE' })).status).toBe(405);
    expect((await call(main.server, `${JAR}?x`)).status).toBe(403);
    expect(seen).toHaveLength(0);
  });
});

describe('forwarding', () => {
  it('passes sign-in requests on without Origin, Referer or cookies', async () => {
    const response = await call(main.server, TOKEN, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/x-www-form-urlencoded',
        Authorization: 'Bearer secret-token',
        Origin: 'https://rustcraft.online',
        Referer: 'https://rustcraft.online/',
        Cookie: 'a=b',
        'X-Forwarded-For': '10.0.0.1',
      },
      body: 'grant_type=refresh_token&refresh_token=secret-refresh',
    });
    expect(response.status).toBe(200);
    const forwarded = seen[0]!;
    expect(forwarded.method).toBe('POST');
    expect(forwarded.url).toBe('/consumers/oauth2/v2.0/token');
    expect(forwarded.body).toBe('grant_type=refresh_token&refresh_token=secret-refresh');
    expect(forwarded.headers.host).toBe('login.microsoftonline.com');
    expect(forwarded.headers.authorization).toBe('Bearer secret-token');
    for (const name of ['origin', 'referer', 'cookie', 'x-forwarded-for']) expect(forwarded.headers[name]).toBeUndefined();
    expect(response.headers['set-cookie']).toBeUndefined();
    expect(response.headers['access-control-allow-origin']).toBeUndefined();
    expect(response.headers['cache-control']).toBe('no-store');
  });

  it('streams Mojang files without letting anyone cache them', async () => {
    const response = await call(main.server, JAR);
    expect(response.status).toBe(200);
    expect(seen[0]!.url).toBe('/com/google/code/gson/gson/2.14.0/gson-2.14.0.jar');
    expect(response.headers['cache-control']).toBe('no-store');
    expect((await call(main.server, OBJECT)).status).toBe(200);
  });

  it('refuses oversized request bodies', async () => {
    const big = 'x'.repeat(MAX_REQUEST_BODY + 1);
    expect((await call(main.server, TOKEN, { method: 'POST', body: big })).status).toBe(413);
  });

  it('does not follow redirects', async () => {
    mode = 'redirect';
    const response = await call(main.server, JAR);
    expect(response.status).toBe(502);
    expect(response.headers.location).toBeUndefined();
    expect(seen).toHaveLength(1);
  });

  it('times out slow upstreams and caps sign-in responses', async () => {
    const quick = await front({ timeouts: { auth: 200, filesFirstByte: 200 }, maxAuthResponse: 1024 });
    mode = 'stall';
    expect((await call(quick.server, PROFILE)).status).toBe(504);
    expect((await call(quick.server, JAR)).status).toBe(504);
    mode = 'big';
    expect((await call(quick.server, PROFILE)).status).toBe(502);
    expect((await call(quick.server, JAR)).status).toBe(200); // files are not capped
  });
});

describe('callers', () => {
  it('serves only our own pages', async () => {
    expect(SITE_ORIGIN).toBe(SITE_URL);
    expect((await call(main.server, JAR, { headers: { Origin: 'https://rustcraft.online' } })).status).toBe(200);
    expect((await call(main.server, JAR, { headers: { Origin: `http://127.0.0.1:${port(main.server)}` } })).status).toBe(200);
    expect((await call(main.server, JAR, { headers: { 'Sec-Fetch-Site': 'same-origin' } })).status).toBe(200);
    expect((await call(main.server, JAR, { headers: { Origin: 'https://evil.example' } })).status).toBe(403);
    expect((await call(main.server, JAR, { headers: { 'Sec-Fetch-Site': 'cross-site' } })).status).toBe(403);
  });

  it('reads forwarded client addresses only behind a trusted proxy', () => {
    const req = (headers: Record<string, string>) => ({ headers, socket: { remoteAddress: '10.9.9.9' } }) as unknown as IncomingMessage;
    expect(clientAddress(req({ 'x-forwarded-for': '1.1.1.1' }), false)).toBe('10.9.9.9');
    expect(clientAddress(req({ 'x-forwarded-for': '6.6.6.6, 1.1.1.1' }), true)).toBe('1.1.1.1');
    expect(clientAddress(req({ 'cf-connecting-ip': '2.2.2.2', 'x-forwarded-for': '1.1.1.1' }), true)).toBe('2.2.2.2');
    expect(clientAddress(req({}), true)).toBe('10.9.9.9');
  });
});

describe('rate limits', () => {
  it('answers 429 with Retry-After once a bucket is empty, and refills', async () => {
    let clock = 0;
    const limited = await front({ limits: { auth: { burst: 2, perMinute: 6 } }, now: () => clock });
    expect((await call(limited.server, PROFILE)).status).toBe(200);
    expect((await call(limited.server, PROFILE)).status).toBe(200);
    const refused = await call(limited.server, PROFILE);
    expect(refused.status).toBe(429);
    expect(refused.headers['retry-after']).toBe('10');
    // Files have their own bucket.
    expect((await call(limited.server, JAR)).status).toBe(200);
    clock += 10_000;
    expect((await call(limited.server, PROFILE)).status).toBe(200);
    expect((await call(limited.server, PROFILE)).status).toBe(429);
  });

  it('caps concurrent upstream requests per client', async () => {
    const limited = await front({ maxConcurrentPerClient: 1, timeouts: { filesFirstByte: 500 } });
    mode = 'stall';
    const first = call(limited.server, JAR);
    await new Promise((r) => setTimeout(r, 100));
    const second = await call(limited.server, OBJECT);
    expect(second.status).toBe(429);
    expect(second.headers['retry-after']).toBe('1');
    expect((await first).status).toBe(504);
    mode = 'ok';
    expect((await call(limited.server, OBJECT)).status).toBe(200);
  });

  it('keeps memory bounded by dropping idle buckets', () => {
    let clock = 0;
    const limiter = new RateLimiter({ auth: { burst: 2, perMinute: 60 }, files: { burst: 2, perMinute: 60 } }, () => clock);
    for (let i = 0; i < 1000; i++) limiter.take(`10.0.${i >> 8}.${i & 255}`, 'auth');
    expect(limiter.size).toBe(1000);
    clock += 120_000;
    limiter.take('10.1.0.1', 'auth');
    expect(limiter.size).toBe(1);
  });
});

describe('logging', () => {
  it('logs no paths, tokens or bodies', () => {
    expect(main.logs.length).toBeGreaterThan(0);
    for (const line of main.logs) {
      expect(line).toMatch(/^proxy [A-Z]+ \S+ \d{3} \d+ms$/);
      expect(line).not.toMatch(/secret|token|oauth2|gson|profile|example/);
    }
  });
});
