// Runs a Java program on rjava.wasm in headless Chrome, against a running dev
// server (pnpm dev), and streams its output here.
//   node e2e/jvm.mjs [-Xmx..] [-cp <path>] <MainClass> [args...]
// Host paths on the class path are visible read-only (dev-fs); the JDK image is
// .scratch/jre25, the working directory an in-memory /work.
import puppeteer from 'puppeteer-core';
import { dirname, resolve } from 'node:path';

const URL = process.env.LAUNCHER_URL ?? 'http://192.168.10.254:5173/';
const CHROME = process.env.CHROME ?? '/usr/bin/google-chrome-stable';
const repo = resolve(dirname(new globalThis.URL(import.meta.url).pathname), '../..');
// WASM=<path>: another build of rjava.wasm.
const wasm = `${URL}dev-fs${process.env.WASM ? resolve(process.env.WASM) : `${repo}/vm/target/wasm32-wasip1-threads/${process.env.PROFILE ?? 'release'}/rjava.wasm`}`;

// Relative class path entries are resolved against the current directory.
const args = process.argv.slice(2);
const cp = args.findIndex((a) => a === '-cp' || a === '-classpath');
if (cp >= 0) args[cp + 1] = args[cp + 1].split(':').map((p) => resolve(p)).join(':');

const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  // CHROME_FLAGS: extra Chrome flags (e.g. --js-flags=--liftoff-only).
  args: [
    '--enable-unsafe-webgpu',
    '--window-size=1280,1000',
    // SharedArrayBuffer needs a secure context: a dev server on a LAN address
    // (not localhost) is treated as one.
    ...(new globalThis.URL(URL).hostname === 'localhost' ? [] : [`--unsafely-treat-insecure-origin-as-secure=${new globalThis.URL(URL).origin}`]),
    ...(process.env.CHROME_FLAGS ?? '').split(' ').filter(Boolean),
  ],
  // SCALE: the device pixel ratio.
  defaultViewport: { width: 1280, height: 1000, deviceScaleFactor: Number(process.env.SCALE ?? 1) },
  protocolTimeout: 0,
});
let code = 1;
try {
  const page = await browser.newPage();
  page.on('console', (msg) => {
    const text = msg.text();
    const match = /^\[(\d)\] ([\s\S]*)$/.exec(text);
    if (match) (match[1] === '2' ? process.stderr : process.stdout).write(`${match[2]}\n`);
    else process.stderr.write(`[browser ${msg.type()}] ${text}\n`);
  });
  page.on('pageerror', (error) => process.stderr.write(`[pageerror] ${error.message}\n`));
  // The browser's own messages (e.g. WebGL errors) from workers.
  const workers = [];
  page.on('workercreated', (worker) => {
    const client = worker.client;
    workers.push(client);
    client.on('Log.entryAdded', ({ entry }) => process.stderr.write(`[browser ${entry.source} ${entry.level}] ${entry.text}\n`));
    void client.send('Log.enable').catch(() => {});
  });
  // No hot reload (vite's client talks over a WebSocket): editing sources
  // must not restart the program mid-run.
  await page.evaluateOnNewDocument(() => {
    window.WebSocket = class {
      addEventListener() {}
      close() {}
      send() {}
    };
  });
  await page.goto(`${URL}jvm.html`);
  await page.waitForFunction(() => typeof window.runJvm === 'function');
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => key.startsWith('RJAVA_')),
  );
  // SCREENSHOTS=<dir>: a screenshot of the page every 20 seconds.
  const shots = process.env.SCREENSHOTS;
  let shot = 0;
  const timer = shots && setInterval(() => void page.screenshot({ path: `${shots}/shot-${String(++shot).padStart(3, '0')}.png` }).catch(() => {}), 20_000);
  // CPU_PROFILE=<start s>:<seconds>:<dir>: CPU profiles of every worker (thread)
  // over that window, as <dir>/worker-N.cpuprofile (see e2e/profile-summary.mjs).
  if (process.env.CPU_PROFILE) {
    const [start, seconds, dir] = process.env.CPU_PROFILE.split(':');
    setTimeout(async () => {
      const clients = [...workers];
      await Promise.all(clients.map(async (c) => {
        await c.send('Profiler.enable');
        await c.send('Profiler.setSamplingInterval', { interval: 1000 });
        await c.send('Profiler.start');
      }).map((p) => p.catch(() => {})));
      process.stderr.write(`[e2e] profiling ${clients.length} workers\n`);
      setTimeout(async () => {
        const { writeFileSync } = await import('node:fs');
        await Promise.all(clients.map(async (c, i) => {
          const { profile } = await c.send('Profiler.stop');
          writeFileSync(`${dir}/worker-${i}.cpuprofile`, JSON.stringify(profile));
        }).map((p) => p.catch(() => {})));
        process.stderr.write(`[e2e] profiles written to ${dir}\n`);
      }, Number(seconds) * 1000);
    }, Number(start) * 1000);
  }
  // CLICKS=<s>:<x>,<y>;...: clicks at page coordinates after s seconds.
  for (const click of (process.env.CLICKS ?? '').split(';').filter(Boolean)) {
    const [at, xy] = click.split(':');
    const [x, y] = xy.split(',').map(Number);
    setTimeout(() => void page.mouse.click(x, y).then(() => process.stderr.write(`[e2e] clicked ${x},${y}\n`)), Number(at) * 1000);
  }
  // RESIZE_AT=<seconds>: then make the page narrower (as opening DevTools does).
  if (process.env.RESIZE_AT) {
    setTimeout(() => void page.setViewport({ width: 900, height: 800, deviceScaleFactor: Number(process.env.SCALE ?? 1) }), Number(process.env.RESIZE_AT) * 1000);
  }
  code = await page.evaluate(
    (request) => window.runJvm(request),
    {
      wasm,
      args: ['rjava', '--modules', `${repo}/.scratch/jre25/lib/modules`, ...args],
      env: { PWD: '/work', HOME: '/work', ...env },
      // MEMORY_PAGES: the initial memory size (64 KiB pages).
      initialPages: process.env.MEMORY_PAGES ? Number(process.env.MEMORY_PAGES) : undefined,
      mounts: [
        // ROOT=<host dir>: the root directory (and the working directory) is
        // that directory, with writes kept in memory.
        ...(process.env.ROOT ? [{ path: '/', kind: 'overlay', url: `${URL}dev-fs${resolve(process.env.ROOT)}` }] : []),
        { path: '/home', kind: 'http', url: `${URL}dev-fs/home` },
        { path: '/mnt', kind: 'http', url: `${URL}dev-fs/mnt` },
        // GAME=<host dir>: /work is that directory, with writes kept in memory.
        process.env.GAME
          ? { path: '/work', kind: 'overlay', url: `${URL}dev-fs${resolve(process.env.GAME)}` }
          : { path: '/work', kind: 'memory' },
      ],
    },
  );
  if (timer) clearInterval(timer);
} finally {
  await browser.close();
}
process.exit(code);
