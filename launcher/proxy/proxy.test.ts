import { createServer, request, type IncomingHttpHeaders, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { SITE_URL } from '../src/brand.ts';
import { createProxy, MAX_REQUEST_BODY, SITE_ORIGIN } from './proxy.ts';

let upstream: Server;
let front: Server;
let seen: { method?: string; url?: string; headers: IncomingHttpHeaders; body: string }[] = [];
const logs: string[] = [];
const port = (server: Server) => (server.address() as AddressInfo).port;

function call(path: string, init: { method?: string; headers?: Record<string, string>; body?: string } = {}) {
  return new Promise<{ status: number; headers: IncomingHttpHeaders; body: string }>((resolve, reject) => {
    const req = request({ host: '127.0.0.1', port: port(front), path, method: init.method ?? 'GET', headers: init.headers }, (res) => {
      let body = '';
      res.on('data', (c: Buffer) => (body += c));
      res.on('end', () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body }));
    });
    req.on('error', reject);
    req.end(init.body);
  });
}

beforeAll(async () => {
  upstream = createServer((req, res) => {
    let body = '';
    req.on('data', (c: Buffer) => (body += c));
    req.on('end', () => {
      seen.push({ method: req.method, url: req.url, headers: req.headers, body });
      res.writeHead(200, {
        'Content-Type': 'application/json',
        'Set-Cookie': 'session=1',
        'Cache-Control': 'public, max-age=86400',
        'Access-Control-Allow-Origin': '*',
      });
      res.end(JSON.stringify({ ok: true, size: body.length }));
    });
  });
  await new Promise<void>((r) => upstream.listen(0, '127.0.0.1', r));
  const proxy = createProxy({ upstream: () => ({ protocol: 'http:', hostname: '127.0.0.1', port: port(upstream) }), log: (l) => logs.push(l) });
  front = createServer((req, res) => {
    if (!proxy(req, res)) res.writeHead(404).end();
  });
  await new Promise<void>((r) => front.listen(0, '127.0.0.1', r));
});

afterAll(() => {
  upstream.close();
  front.close();
});

describe('production proxy', () => {
  it('forwards sign-in requests without Origin, Referer or cookies', async () => {
    seen = [];
    const response = await call('/proxy/login.microsoftonline.com/consumers/oauth2/v2.0/token?x=1', {
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
    expect(forwarded.url).toBe('/consumers/oauth2/v2.0/token?x=1');
    expect(forwarded.body).toBe('grant_type=refresh_token&refresh_token=secret-refresh');
    expect(forwarded.headers.host).toBe('login.microsoftonline.com');
    expect(forwarded.headers.authorization).toBe('Bearer secret-token');
    for (const name of ['origin', 'referer', 'cookie', 'x-forwarded-for']) expect(forwarded.headers[name]).toBeUndefined();
    expect(response.headers['set-cookie']).toBeUndefined();
    expect(response.headers['access-control-allow-origin']).toBeUndefined();
    expect(response.headers['cache-control']).toBe('no-store');
  });

  it('streams Mojang files without letting anyone cache them', async () => {
    seen = [];
    const response = await call('/proxy/libraries.minecraft.net/com/google/gson.jar');
    expect(response.status).toBe(200);
    expect(seen[0]!.url).toBe('/com/google/gson.jar');
    expect(response.headers['cache-control']).toBe('no-store');
  });

  it('refuses other hosts, methods and oversized bodies', async () => {
    expect((await call('/proxy/example.com/')).status).toBe(404);
    expect((await call('/proxy/libraries.minecraft.net/x', { method: 'POST', body: 'x' })).status).toBe(405);
    expect((await call('/proxy/api.minecraftservices.com/x', { method: 'DELETE' })).status).toBe(405);
    const big = 'x'.repeat(MAX_REQUEST_BODY + 1);
    expect((await call('/proxy/api.minecraftservices.com/x', { method: 'POST', body: big })).status).toBe(413);
  });

  it('serves only our own pages', async () => {
    expect(SITE_ORIGIN).toBe(SITE_URL);
    const path = '/proxy/libraries.minecraft.net/x.jar';
    expect((await call(path, { headers: { Origin: 'https://rustcraft.online' } })).status).toBe(200);
    expect((await call(path, { headers: { Origin: `http://127.0.0.1:${port(front)}` } })).status).toBe(200);
    expect((await call(path, { headers: { 'Sec-Fetch-Site': 'same-origin' } })).status).toBe(200);
    expect((await call(path, { headers: { Origin: 'https://evil.example' } })).status).toBe(403);
    expect((await call(path, { headers: { 'Sec-Fetch-Site': 'cross-site' } })).status).toBe(403);
  });

  it('logs no paths, tokens or bodies', () => {
    expect(logs.length).toBeGreaterThan(0);
    for (const line of logs) {
      expect(line).toMatch(/^proxy [A-Z]+ \S+ \d{3} \d+ms$/);
      expect(line).not.toMatch(/secret|token|oauth2|gson/);
    }
  });
});
