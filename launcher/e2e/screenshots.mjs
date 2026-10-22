// Screenshots of the launcher page for review: desktop/narrow, en/ru, dark/light, device code, progress.
//   PROFILE_DIR=<chrome profile> LAUNCHER_URL=http://localhost:5174/ node e2e/screenshots.mjs <dir>
import puppeteer from 'puppeteer-core';
const URL = process.env.LAUNCHER_URL ?? 'http://localhost:5174/';
const out = process.argv[2];
const browser = await puppeteer.launch({
  executablePath: '/usr/bin/google-chrome-stable', headless: true,
  userDataDir: `${process.env.PROFILE_DIR}`, args: ['--window-size=1280,900'], protocolTimeout: 0,
});
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
try {
  const page = await browser.newPage();
  page.on('pageerror', (e) => console.error('[pageerror]', e.message));
  page.on('console', (m) => m.type() === 'error' && console.error('[console]', m.text()));
  await page.evaluateOnNewDocument(() => { window.WebSocket = class { addEventListener() {} close() {} send() {} }; });
  const open = async (lang, width, height, scheme) => {
    await page.setViewport({ width, height, deviceScaleFactor: 1 });
    await page.emulateMediaFeatures([{ name: 'prefers-color-scheme', value: scheme }]);
    await page.goto(URL);
    await page.evaluate((l) => { localStorage.setItem('mcweb.lang', l); localStorage.removeItem('mcweb.account'); localStorage.removeItem('mcweb.msClientId'); localStorage.setItem('mcweb.version', '26.3'); }, lang);
    await page.reload();
    await page.waitForFunction(() => document.querySelector('#compat').textContent.includes('26.3'), { timeout: 30000 });
    await sleep(500);
  };
  for (const [lang, scheme] of [['en', 'dark'], ['ru', 'dark'], ['en', 'light']]) {
    await open(lang, 1280, 900, scheme);
    await page.screenshot({ path: `${out}/desktop-${lang}-${scheme}.png`, fullPage: true });
  }
  await open('ru', 390, 844, 'dark');
  await page.screenshot({ path: `${out}/narrow-ru-dark.png`, fullPage: true });
  await open('en', 390, 844, 'light');
  await page.screenshot({ path: `${out}/narrow-en-light.png`, fullPage: true });
  // Device code sign-in.
  await open('en', 1280, 900, 'dark');
  await page.click('#login');
  await page.waitForFunction(() => document.querySelector('#device-code').textContent.length > 0, { timeout: 20000 });
  await sleep(300);
  await page.screenshot({ path: `${out}/desktop-en-device-code.png` });
  await page.click('#device-cancel');
  // Download progress (a full check of the installed version).
  await page.click('#verify');
  await page.waitForFunction(() => !document.querySelector('#progress').hidden && /\d+ \/ \d+/.test(document.querySelector('#progress-text').textContent));
  await sleep(1500);
  await page.screenshot({ path: `${out}/desktop-en-progress.png` });
  if (await page.evaluate(() => !document.querySelector('#cancel').hidden)) await page.click('#cancel');
  await sleep(500);
  // Signed in (a stand-in account: no Microsoft calls).
  await page.evaluate(() => {
    const far = Date.now() + 86_400_000;
    localStorage.setItem('mcweb.account', JSON.stringify({
      clientId: 'e2e', msa: { accessToken: 'e2e', refreshToken: 'e2e', expiresAt: far }, minecraft: { accessToken: 'e2e', expiresAt: far },
      xuid: '0', entitlements: ['game_minecraft'], profile: { id: '8c0e91f026e835ffa0b655ef0a301da5', name: 'Steve_RC' }, licensed: true, checkedAt: Date.now(),
    }));
  });
  await page.reload();
  await page.waitForFunction(() => !document.querySelector('#play').disabled, { timeout: 30000 });
  await page.screenshot({ path: `${out}/desktop-en-signed-in.png` });
  await page.setViewport({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/narrow-en-signed-in.png` });
  await page.setViewport({ width: 1280, height: 900 });
  // Settings expanded.
  await page.evaluate(() => document.querySelectorAll('.settings-card details.group').forEach((d) => { if (!d.hidden) d.open = true; }));
  await page.setViewport({ width: 1280, height: 1400 });
  await page.screenshot({ path: `${out}/desktop-en-settings-open.png`, fullPage: true });
  // Language switch at runtime.
  await page.select('#lang', 'ru');
  await sleep(300);
  await page.evaluate(() => localStorage.removeItem('mcweb.account'));
  console.log('after switch:', await page.evaluate(() => [document.documentElement.lang, document.querySelector('#play').textContent, document.querySelector('#play-hint').textContent, document.querySelector('#disclaimer-local').textContent].join(' | ')));
} finally {
  await browser.close();
}
