// Production server for the launcher: the CORS proxy (proxy.ts) and,
// optionally, the built site (`pnpm build` → dist/) with the cross-origin
// isolation headers the JVM needs (SharedArrayBuffer).
//
//   node proxy/server.ts                     proxy only (put the static site elsewhere)
//   STATIC_DIR=dist node proxy/server.ts     proxy + site
//
// Env: PORT (8080), HOST (127.0.0.1), STATIC_DIR, ALLOWED_ORIGINS (comma-
// separated origins whose pages may use the proxy besides the server's own;
// default https://rustcraft.online), TLS_CERT + TLS_KEY (PEM files; otherwise
// run it behind an HTTPS reverse proxy — the site must be served over HTTPS).
// No dependencies; Node ≥ 23.6 runs this TypeScript as is.

import { createReadStream, readFileSync } from 'node:fs';
import { stat } from 'node:fs/promises';
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { createServer as createTlsServer } from 'node:https';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { createProxy } from './proxy.ts';

const port = Number(process.env.PORT ?? 8080);
const host = process.env.HOST ?? '127.0.0.1';
const staticDir = process.env.STATIC_DIR ? resolve(process.env.STATIC_DIR) : null;

const TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.wasm': 'application/wasm',
  '.txt': 'text/plain; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
};

const allowedOrigins = process.env.ALLOWED_ORIGINS?.split(',').map((origin) => origin.trim()).filter(Boolean);
const proxy = createProxy({ allowedOrigins });

async function serveStatic(req: IncomingMessage, res: ServerResponse) {
  const isolation = {
    'Cross-Origin-Opener-Policy': 'same-origin',
    'Cross-Origin-Embedder-Policy': 'require-corp',
    'Cross-Origin-Resource-Policy': 'same-origin',
    'X-Content-Type-Options': 'nosniff',
  };
  if (!staticDir || (req.method !== 'GET' && req.method !== 'HEAD')) {
    res.writeHead(404, isolation).end();
    return;
  }
  const pathname = decodeURIComponent(new URL(req.url ?? '/', 'http://x').pathname);
  let file = normalize(join(staticDir, pathname.endsWith('/') ? `${pathname}index.html` : pathname));
  if (file !== staticDir && !file.startsWith(staticDir + sep)) {
    res.writeHead(404, isolation).end();
    return;
  }
  let info = await stat(file).catch(() => null);
  if (info?.isDirectory()) {
    file = join(file, 'index.html');
    info = await stat(file).catch(() => null);
  }
  if (!info?.isFile()) {
    res.writeHead(404, isolation).end();
    return;
  }
  const type = TYPES[extname(file)] ?? 'application/octet-stream';
  // Hashed assets never change; pages and rjava.wasm are revalidated.
  const cache = file.includes(`${sep}assets${sep}`) ? 'public, max-age=31536000, immutable' : 'no-cache';
  res.writeHead(200, { ...isolation, 'Content-Type': type, 'Content-Length': info.size, 'Cache-Control': cache });
  if (req.method === 'HEAD') res.end();
  else createReadStream(file).pipe(res);
}

function handle(req: IncomingMessage, res: ServerResponse) {
  if (proxy(req, res)) return;
  serveStatic(req, res).catch(() => {
    if (!res.headersSent) res.writeHead(500).end();
  });
}

const tls = process.env.TLS_CERT && process.env.TLS_KEY;
const server = tls
  ? createTlsServer({ cert: readFileSync(process.env.TLS_CERT!), key: readFileSync(process.env.TLS_KEY!) }, handle)
  : createServer(handle);
server.listen(port, host, () => {
  console.log(`launcher ${tls ? 'https' : 'http'}://${host}:${port}/ — proxy${staticDir ? ` + site from ${staticDir}` : ' only'}`);
});
