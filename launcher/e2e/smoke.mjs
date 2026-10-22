// Browser smoke test for the launcher against a running dev server (pnpm dev).
//   node e2e/smoke.mjs          partial install → cancel → resume (≈100 MB)
//   node e2e/smoke.mjs --full   complete install of the latest release (≈600 MB)
// Uses the system Chrome; the profile (and its OPFS) lives in .e2e-profile/.
import puppeteer from 'puppeteer-core';

const URL = process.env.LAUNCHER_URL ?? 'http://localhost:5173/';
const CHROME = process.env.CHROME ?? '/usr/bin/google-chrome-stable';
const full = process.argv.includes('--full');
const PARTIAL_FILES = 300;

const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  userDataDir: new globalThis.URL('../.e2e-profile', import.meta.url).pathname,
  args: ['--enable-unsafe-webgpu'],
});
const errors = [];
try {
  const page = await browser.newPage();
  page.on('pageerror', (error) => errors.push(`pageerror: ${error.message}`));
  page.on('console', (msg) => msg.type() === 'error' && errors.push(`console: ${msg.text()}`));

  await page.goto(URL);
  await page.waitForFunction(() => document.querySelectorAll('#version option').length > 0, { timeout: 30_000 });
  const env = await page.$$eval('#env .badge', (badges) => badges.map((b) => b.textContent));
  console.log('environment:', env.join(' | '));
  console.log('selected version:', await page.$eval('#version', (s) => s.value));

  const progress = () =>
    page.evaluate(() => ({
      text: document.querySelector('#progress-text').textContent,
      status: document.querySelector('#status').textContent,
      failures: document.querySelector('#failures').hidden ? '' : document.querySelector('#failures-list').textContent,
    }));
  // Polls from Node (a long in-page waitForFunction trips puppeteer's protocol timeout)
  // and logs progress while waiting.
  const until = async (predicate, timeout) => {
    const deadline = Date.now() + timeout;
    let lastLog = 0;
    for (;;) {
      const state = await progress();
      if (predicate(state)) return state;
      if (Date.now() > deadline) throw new Error(`Timed out; last state: ${JSON.stringify(state)}`);
      if (Date.now() - lastLog > 10_000) {
        lastLog = Date.now();
        console.log('  …', state.text, '|', state.status);
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  };
  const fileCount = (state) => Number(/^(\d+)/.exec(state.text)?.[1] ?? 0);
  const failed = (state) => /Не удалось/.test(state.status);
  const waitForStatus = (pattern, timeout) => until((state) => pattern.test(state.status), timeout);

  await page.click('#install');
  if (full) {
    await waitForStatus(/установлена|отменена|Не удалось/, 30 * 60_000);
    console.log('install:', await progress());
    console.log('plan:', await page.$eval('#plan-summary', (dl) => dl.innerText.replaceAll('\n', ' ')));
  } else {
    const first = await until((state) => fileCount(state) >= PARTIAL_FILES || failed(state), 10 * 60_000);
    console.log('first run before cancel:', first);
    await page.click('#cancel');
    await waitForStatus(/отменена|Не удалось/, 30_000);
    console.log('after cancel:', await progress());

    await page.click('#install');
    await until((state) => /уже установлено/.test(state.text) || failed(state), 5 * 60_000);
    await page.click('#cancel');
    await waitForStatus(/отменена|Не удалось/, 30_000);
    console.log('resume run:', await progress());
    console.log('plan shown:', await page.$eval('#plan', (p) => !p.hidden));
  }
  console.log('storage:', await page.$eval('#storage', (p) => p.textContent));
} finally {
  await browser.close();
}
if (errors.length) {
  console.error('browser errors:\n  ' + errors.join('\n  '));
  process.exit(1);
}
