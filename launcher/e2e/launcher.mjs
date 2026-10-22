// End-to-end check of the launcher page in headless Chrome, against a running
// dev server:
//   node e2e/launcher.mjs install [version]   download a version into OPFS through the
//                                             page's "Проверить файлы" button, then re-hash
//                                             files in OPFS against Mojang's SHA-1s
//   node e2e/launcher.mjs ui                  the main page: versions, compatibility, Play gating,
//                                             starting (and cancelling) a Microsoft device code sign-in
//   node e2e/launcher.mjs play [seconds]      the Play button end to end with a stand-in account
//                                             (fake, unexpired tokens: no Microsoft calls) and EULA ticked
//   node e2e/launcher.mjs fs                  exercise the OPFS mount backend in a worker
//                                             (lazy asset fetch, write, rename, unlink)
//   node e2e/launcher.mjs launch [seconds]    start the installed version from OPFS on our VM
//                                             (no Microsoft account: offline token) and watch
//                                             it for a while; SCREENSHOTS=<dir> saves screens
// LAUNCHER_URL (default http://localhost:5173/), CHROME, PROFILE_DIR (Chrome profile, so
// OPFS survives between runs; default .scratch/launcher-agent/chrome-profile).
import puppeteer from 'puppeteer-core';
import { dirname, resolve } from 'node:path';

const URL = process.env.LAUNCHER_URL ?? 'http://localhost:5173/';
const CHROME = process.env.CHROME ?? '/usr/bin/google-chrome-stable';
const repo = resolve(dirname(new globalThis.URL(import.meta.url).pathname), '../..');
const profile = process.env.PROFILE_DIR ?? `${repo}/.scratch/launcher-agent/chrome-profile`;
const [mode = 'install', arg] = process.argv.slice(2);

const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  userDataDir: profile,
  args: ['--enable-unsafe-webgpu', '--window-size=1280,800'],
  defaultViewport: { width: 1280, height: 800 },
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
  await page.evaluateOnNewDocument(() => {
    window.WebSocket = class {
      addEventListener() {}
      close() {}
      send() {}
    };
  });
  await page.goto(URL);
  await page.waitForFunction(() => document.querySelector('#version option'), { timeout: 30_000 });

  if (mode === 'install') {
    const version = arg ?? '26.3';
    await page.select('#version', version);
    await page.waitForFunction(() => !document.querySelector('#verify').disabled);
    const started = Date.now();
    await page.click('#verify');
    let last = '';
    for (;;) {
      await new Promise((r) => setTimeout(r, 2000));
      const state = await page.evaluate(() => ({
        status: document.querySelector('#status').textContent,
        progress: document.querySelector('#progress-text').textContent,
        error: document.querySelector('#status').classList.contains('error'),
        failures: document.querySelector('#failures-list').textContent,
        busy: document.querySelector('#verify').disabled,
      }));
      const line = `${state.progress} | ${state.status}`;
      if (line !== last) process.stderr.write(`[e2e] ${line}\n`);
      last = line;
      if (!state.busy) {
        if (state.error) {
          process.stderr.write(`[e2e] install failed:\n${state.failures}\n`);
          break;
        }
        process.stderr.write(`[e2e] installed in ${Math.round((Date.now() - started) / 1000)} s\n`);
        code = 0;
        break;
      }
    }
    if (code === 0) {
      // Independent check: re-hash files in OPFS against the published SHA-1s.
      const report = await page.evaluate(async (id) => {
        const root = await (await navigator.storage.getDirectory()).getDirectoryHandle('minecraft');
        const file = async (path) => {
          let dir = root;
          const parts = path.split('/');
          const name = parts.pop();
          for (const part of parts) dir = await dir.getDirectoryHandle(part);
          return (await dir.getFileHandle(name)).getFile();
        };
        const sha1 = async (blob) =>
          Array.from(new Uint8Array(await crypto.subtle.digest('SHA-1', await blob.arrayBuffer())), (b) => b.toString(16).padStart(2, '0')).join('');
        const version = JSON.parse(await (await file(`versions/${id}/${id}.json`)).text());
        const checks = [[`versions/${id}/${id}.jar`, version.downloads.client.sha1]];
        for (const lib of version.libraries.filter((l) => l.downloads?.artifact && !l.rules && !/natives|linux-|osx-|windows-/.test(l.name)).slice(0, 5)) {
          checks.push([`libraries/${lib.downloads.artifact.path}`, lib.downloads.artifact.sha1]);
        }
        const component = version.javaVersion.component;
        const runtime = JSON.parse(await (await file(`runtime/${component}.json`)).text());
        for (const path of ['lib/modules', 'conf/security/java.security', 'lib/tzdb.dat']) {
          checks.push([`runtime/${component}/${path}`, runtime.files[path].downloads.raw.sha1]);
        }
        const index = JSON.parse(await (await file(`assets/indexes/${version.assetIndex.id}.json`)).text());
        const icon = index.objects['icons/icon_32x32.png'];
        if (icon) checks.push([`assets/objects/${icon.hash.slice(0, 2)}/${icon.hash}`, icon.hash]);
        const results = [];
        for (const [path, expected] of checks) {
          try {
            const blob = await file(path);
            const actual = await sha1(blob);
            results.push({ path, size: blob.size, ok: actual === expected });
          } catch (error) {
            results.push({ path, ok: false, error: String(error) });
          }
        }
        return results;
      }, arg ?? '26.3');
      for (const r of report) process.stderr.write(`[e2e] ${r.ok ? 'OK ' : 'BAD'} ${r.path} ${r.size ?? r.error}\n`);
      if (report.some((r) => !r.ok)) code = 1;
    }
  } else if (mode === 'ui') {
    const shots = process.env.SCREENSHOTS;
    const state = () =>
      page.evaluate(() => ({
        selected: document.querySelector('#version').value,
        options: document.querySelector('#version').options.length,
        compat: document.querySelector('#compat').textContent,
        play: document.querySelector('#play').disabled,
        hint: document.querySelector('#play-hint').textContent,
        account: document.querySelector('#account-status').textContent,
        code: document.querySelector('#device-code').textContent,
        link: document.querySelector('#device-link').href,
      }));
    const compatShown = () =>
      page.waitForFunction(() => {
        const text = document.querySelector('#compat').textContent;
        return text.startsWith(document.querySelector('#version').value) || /^[✓✗] /.test(text) && text.includes(document.querySelector('#version').value);
      }, { timeout: 15_000 });
    await page.select('#version', '26.3');
    await compatShown();
    process.stderr.write(`[e2e] ${JSON.stringify(await state())}\n`);
    const noId = await page.evaluate(() => ({
      disabled: document.querySelector('#login').disabled,
      notice: !document.querySelector('#no-client-id').hidden,
      note: document.querySelector('#client-id-note').textContent,
      title: document.title,
      footer: document.querySelector('#disclaimer').textContent,
    }));
    process.stderr.write(`[e2e] client id state: ${JSON.stringify(noId)}\n`);
    if (noId.disabled) {
      // No VITE_MS_CLIENT_ID in this build: E2E_CLIENT_ID goes into the settings field.
      if (!process.env.E2E_CLIENT_ID) {
        if (process.env.SCREENSHOTS) await page.screenshot({ path: `${process.env.SCREENSHOTS}/ui-no-client-id.png`, fullPage: true });
        code = noId.notice ? 0 : 1;
        throw new Error('done: no client ID configured (set E2E_CLIENT_ID to test sign-in)');
      }
      await page.evaluate(() => (document.querySelector('details:has(#client-id)').open = true));
      await page.type('#client-id', process.env.E2E_CLIENT_ID);
      await page.click('#client-id-save');
    }
    await page.click('#login');
    await page.waitForFunction(() => document.querySelector('#device-code').textContent.length > 0 || document.querySelector('#account-status').classList.contains('error'), { timeout: 20_000 });
    const signingIn = await state();
    process.stderr.write(`[e2e] ${JSON.stringify(signingIn)}\n`);
    if (shots) await page.screenshot({ path: `${shots}/ui-device-code.png`, fullPage: true });
    await page.click('#device-cancel');
    await page.waitForFunction(() => !document.querySelector('#account-signed-out').hidden);
    process.stderr.write(`[e2e] after cancel: ${JSON.stringify(await state())}\n`);
    await page.click('label.switch');
    await page.select('#version', '1.21.11');
    await compatShown();
    process.stderr.write(`[e2e] snapshots on: ${JSON.stringify(await state())}\n`);
    if (shots) await page.screenshot({ path: `${shots}/ui-main.png`, fullPage: true });
    code = /^[A-Z0-9]{6,}$/.test(signingIn.code) ? 0 : 1;
    await page.evaluate(() => localStorage.removeItem('mcweb.msClientId'));
  } else if (mode === 'play') {
    const seconds = Number(arg ?? 120);
    await page.evaluate(() => {
      const far = Date.now() + 86_400_000;
      localStorage.setItem('mcweb.account', JSON.stringify({
        clientId: 'e2e', msa: { accessToken: 'e2e', refreshToken: 'e2e', expiresAt: far }, minecraft: { accessToken: 'e2e-token', expiresAt: far },
        xuid: '0', entitlements: ['game_minecraft'], profile: { id: '8c0e91f026e835ffa0b655ef0a301da5', name: 'E2E' }, licensed: true, checkedAt: Date.now(),
      }));
      localStorage.setItem('mcweb.eula', '1');
      localStorage.setItem('mcweb.version', '26.3');
    });
    await page.reload();
    await page.waitForFunction(() => !document.querySelector('#play').disabled, { timeout: 30_000 });
    await page.click('#play');
    const deadline = Date.now() + seconds * 1000;
    let shown = false;
    while (Date.now() < deadline && !shown) {
      await new Promise((r) => setTimeout(r, 2000));
      shown = await page.evaluate(() => !!document.querySelector('#screen canvas:not([hidden])'));
    }
    if (shown) await new Promise((r) => setTimeout(r, 30_000));
    const status = await page.evaluate(() => document.querySelector('#status').textContent);
    process.stderr.write(`[e2e] window shown: ${shown}; status: ${status}\n`);
    if (process.env.SCREENSHOTS) await page.screenshot({ path: `${process.env.SCREENSHOTS}/play-final.png` });
    await page.evaluate(() => localStorage.removeItem('mcweb.account'));
    code = shown ? 0 : 1;
  } else if (mode === 'fs') {
    const result = await page.evaluate(async (base) => {
      const worker = new Worker(`${base}e2e/opfs-check.js`, { type: 'module' });
      return new Promise((resolve) => {
        worker.onmessage = (e) => resolve(e.data);
        worker.onerror = (e) => resolve([`BAD worker error ${e.message}`]);
      });
    }, URL);
    for (const line of result) process.stderr.write(`[e2e] ${line}\n`);
    code = result.some((line) => line.startsWith('BAD')) ? 1 : 0;
  } else if (mode === 'launch') {
    const seconds = Number(arg ?? 180);
    const shots = process.env.SCREENSHOTS;
    let shot = 0;
    const timer = shots && setInterval(() => void page.screenshot({ path: `${shots}/launch-${String(++shot).padStart(3, '0')}.png` }).catch(() => {}), 20_000);
    // The page's own modules: build the plan and start the game as Play does, with an
    // offline identity instead of a Microsoft account.
    await page.evaluate(async () => {
      const { buildLaunchPlan } = await import('/src/mojang/launchPlan.ts');
      const { vmProcess } = await import('/src/mojang/vm.ts');
      const { readJson } = await import('/src/storage/opfs.ts');
      const { runGame } = await import('/src/ui/game.ts');
      const id = document.querySelector('#version').value;
      const version = await readJson(`versions/${id}/${id}.json`);
      const plan = buildLaunchPlan(version, {
        username: 'Player', uuid: '8c0e91f0-26e8-35ff-a0b6-55ef0a301da5', accessToken: '0', xuid: '0', userType: 'msa', width: 1280, height: 800,
      });
      const config = vmProcess(plan, { heapMb: 1024 });
      console.log(`[e2e] argv: ${config.args.join(' ').slice(0, 400)}…`);
      window.__exit = null;
      await runGame(config, { onStatus: (m) => console.log(`[e2e] ${m}`), onExit: (c) => (window.__exit = c) });
    });
    const deadline = Date.now() + seconds * 1000;
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 2000));
      const exit = await page.evaluate(() => window.__exit);
      if (exit !== null) {
        process.stderr.write(`[e2e] JVM exited with ${exit}\n`);
        code = exit;
        break;
      }
      code = 0;
    }
    if (timer) clearInterval(timer);
    const state = await page.evaluate(() => ({ canvas: !!document.querySelector('#screen canvas:not([hidden])'), title: document.title }));
    process.stderr.write(`[e2e] window shown: ${state.canvas}, title: ${state.title}\n`);
    if (shots) await page.screenshot({ path: `${shots}/launch-final.png` }).catch(() => {});
  }
} catch (error) {
  if (!String(error?.message).startsWith('done:')) throw error;
  process.stderr.write(`[e2e] ${error.message}\n`);
} finally {
  await browser.close();
}
process.exit(code);
