import { createReadStream, promises as fs } from 'node:fs';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { resolve } from 'node:path';
import { defineConfig, type Plugin } from 'vite';
import { createProxy } from './proxy/proxy.ts';

// Cross-origin isolation: required for SharedArrayBuffer, which the JVM's
// threads (Web Workers sharing one WASM memory) will depend on.
const isolationHeaders = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

// The same-origin proxy for the Mojang, Microsoft and Xbox hosts that send no
// usable CORS headers (/proxy/<host>/<path>, see src/mojang/mirror.ts): the
// production code from proxy/proxy.ts, so development behaves like the site
// (route allowlist, Origin check, rate limits, timeouts, no-store).
const corsProxy = (): Plugin => {
  const proxy = createProxy();
  const middleware = (req: IncomingMessage, res: ServerResponse, next: () => void) => void proxy(req, res, next);
  return {
    name: 'cors-proxy',
    configureServer: (server) => void server.middlewares.use(middleware),
    configurePreviewServer: (server) => void server.middlewares.use(middleware),
  };
};

// Our VM build, at the URL the launcher loads it from (src/ui/game.ts).
const RJAVA_WASM = resolve(import.meta.dirname, '../vm/target/wasm32-wasip1-threads/release/rjava.wasm');
const rjavaWasm = (): Plugin => ({
  name: 'rjava-wasm',
  apply: 'serve',
  configureServer(server) {
    server.middlewares.use('/rjava.wasm', async (_req, res) => {
      const stat = await fs.stat(RJAVA_WASM).catch(() => null);
      if (!stat) {
        res.statusCode = 404;
        res.end('rjava.wasm is not built (cargo build in vm/)');
        return;
      }
      res.setHeader('Content-Type', 'application/wasm');
      res.setHeader('Content-Length', stat.size);
      res.setHeader('Cache-Control', 'no-cache');
      createReadStream(RJAVA_WASM).pipe(res);
    });
  },
});
// A build ships it next to the page.
const rjavaWasmAsset = (): Plugin => ({
  name: 'rjava-wasm-asset',
  apply: 'build',
  async generateBundle() {
    const source = await fs.readFile(RJAVA_WASM).catch(() => null);
    if (source) this.emitFile({ type: 'asset', fileName: 'rjava.wasm', source });
    else this.warn(`${RJAVA_WASM} is not built; the launcher cannot start the game without it`);
  },
});

// Development only: read-only host files for the JVM's HTTP mounts (see
// src/jvm/fs.ts) under /dev-fs/<absolute path>: `?stat`, `?list` or the bytes.
// Limited to the repository (the VM build, the JDK image and test files under
// .scratch) and the local Prism Launcher libraries and assets.
const DEV_FS_ROOTS = [
  resolve(import.meta.dirname, '..'),
  '/mnt/data/Apps/PrismLauncher/libraries',
  '/mnt/data/Apps/PrismLauncher/assets',
];
/** A request made directly to this machine by a local or LAN client. */
function isLocalRequest(req: { headers: Record<string, string | string[] | undefined>; socket: { remoteAddress?: string } }): boolean {
  if (req.headers['x-forwarded-for'] || req.headers['forwarded'] || req.headers['cf-connecting-ip']) return false;
  const host = String(req.headers.host ?? '').replace(/:\d+$/, '').replace(/^\[|\]$/g, '');
  const privateAddress = (a: string) =>
    a === 'localhost' || a === '::1' || /^127\./.test(a) || /^10\./.test(a) || /^192\.168\./.test(a) || /^172\.(1[6-9]|2\d|3[01])\./.test(a);
  const remote = (req.socket.remoteAddress ?? '').replace(/^::ffff:/, '');
  return privateAddress(host) && privateAddress(remote);
}

const devFs = (): Plugin => ({
  name: 'dev-fs',
  apply: 'serve',
  configureServer(server) {
    server.middlewares.use('/dev-fs', async (req, res) => {
      // Local files (the game's JAR, Prism's libraries and assets) must never
      // be served to the outside: only to requests made directly to localhost
      // or a private address, not through a domain, tunnel or reverse proxy.
      if (!isLocalRequest(req)) {
        res.statusCode = 403;
        res.end('dev-fs is only available locally');
        return;
      }
      const url = new URL(req.url ?? '/', 'http://localhost');
      const path = resolve(decodeURIComponent(url.pathname));
      const allowed = DEV_FS_ROOTS.some((root) => path === root || path.startsWith(`${root}/`));
      const stat = allowed ? await fs.stat(path).catch(() => null) : null;
      // Parents of the roots are visible as directories (for listings).
      const ancestor = !stat && DEV_FS_ROOTS.some((root) => root.startsWith(path === '/' ? '/' : `${path}/`));
      res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
      if (!stat && !ancestor) {
        res.statusCode = 404;
        res.end();
      } else if (url.searchParams.has('stat')) {
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({ kind: stat?.isFile() ? 'file' : 'dir', size: stat?.size ?? 0, mtime: stat?.mtimeMs ?? 0 }));
      } else if (url.searchParams.has('list')) {
        const names = stat?.isDirectory()
          ? await fs.readdir(path)
          : DEV_FS_ROOTS.filter((root) => root.startsWith(`${path}/`)).map((root) => root.slice(path.length + 1).split('/')[0]);
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify([...new Set(names)]));
      } else if (stat?.isFile()) {
        res.setHeader('Content-Type', 'application/octet-stream');
        res.setHeader('Content-Length', stat.size);
        createReadStream(path).pipe(res);
      } else {
        res.statusCode = 400;
        res.end();
      }
    });
  },
});

export default defineConfig({
  plugins: [corsProxy(), devFs(), rjavaWasm(), rjavaWasmAsset()],
  // jvm.html is the developer console: served by `pnpm dev`, not part of the site.
  build: { rollupOptions: { input: ['index.html'] } },
  // rustcraft.online may point at this machine during development (e.g.
  // through a tunnel or reverse proxy); Vite rejects unknown Host headers.
  server: { headers: isolationHeaders, allowedHosts: ['rustcraft.online', '.rustcraft.online'] },
  preview: { headers: isolationHeaders, allowedHosts: ['rustcraft.online', '.rustcraft.online'] },
  worker: { format: 'es' },
});
