import './style.css';
import { DISCLAIMER, PRODUCT_NAME } from './brand';
import { dashedUuid } from './auth/minecraft';
import { Installer } from './download/installer';
import type { InstallEvent, InstallProgress } from './download/protocol';
import { initLang, lang, LANGUAGES, onLangChange, setLang, t, tr, translate, type Lang, type MessageKey, type Text } from './i18n';
import { errorText } from './i18n/error';
import { manifestCompat, TESTED_VERSIONS, versionCompat, type Compat } from './mojang/compat';
import { fabricProfileId, mergeProfile, type FabricProfile } from './mojang/fabric';
import { buildLaunchPlan, type LaunchOptions, type LaunchPlan } from './mojang/launchPlan';
import { layout } from './mojang/layout';
import { parseMaven } from './mojang/maven';
import { VERSION_MANIFEST_URL } from './mojang/mirror';
import type { VersionJson, VersionManifest, VersionManifestEntry } from './mojang/types';
import { vmProcess } from './mojang/vm';
import { clearDownloads, clearGameDir, readJson, requestPersistence, storageInfo, writeTextIfMissing, type StorageInfo } from './storage/opfs';
import { AccountView } from './ui/account';
import { LoaderView } from './ui/loader';
import { ModsView } from './ui/mods';
import { MultiplayerView } from './ui/multiplayer';
import { $, formatBytes, prefs } from './ui/dom';
import { runGame } from './ui/game';

/** options.txt for a new game directory (the game keeps the rest at its
 *  defaults): shorter render and simulation distances than vanilla's 12, which
 *  the integrated server running in the browser cannot keep up with. */
const DEFAULT_OPTIONS = 'renderDistance:8\nsimulationDistance:5\n';

initLang();

const ui = {
  lang: $<HTMLSelectElement>('lang'),
  blocking: $('blocking'),
  featureWarning: $('feature-warning'),
  env: $('env'),
  systemSummary: $('system-summary'),
  version: $<HTMLSelectElement>('version'),
  snapshots: $<HTMLInputElement>('snapshots'),
  compat: $('compat'),
  memory: $<HTMLSelectElement>('memory'),
  eula: $<HTMLInputElement>('eula'),
  play: $<HTMLButtonElement>('play'),
  verify: $<HTMLButtonElement>('verify'),
  cancel: $<HTMLButtonElement>('cancel'),
  playHint: $('play-hint'),
  progress: $('progress'),
  progressFill: $('progress-fill'),
  progressText: $('progress-text'),
  status: $('status'),
  failures: $<HTMLDetailsElement>('failures'),
  failuresList: $('failures-list'),
  plan: $<HTMLDetailsElement>('plan'),
  planSummary: $('plan-summary'),
  planArgv: $('plan-argv'),
  storage: $('storage'),
  clear: $<HTMLButtonElement>('clear'),
  clearAll: $<HTMLButtonElement>('clear-all'),
  disclaimerLocal: $('disclaimer-local'),
};

let manifest: VersionManifest | null = null;
/** Version JSONs fetched for the compatibility note, by id. */
const versionJsons = new Map<string, Promise<VersionJson>>();
let compat: Compat | null = null;
let compatVersion: VersionJson | null = null;
let compatError: Text | null = null;
let busy = false;
let installer: Installer | null = null;
let status: { text: Text; error: boolean } | null = null;
let progress: InstallProgress | null = null;
let plan: LaunchPlan | null = null;
let storage: StorageInfo | Text | null = null;
const account = new AccountView(() => updatePlayButton());
const multiplayer = new MultiplayerView();
const mods = new ModsView(() => loader.render());
const loader = new LoaderView(() => onLoaderChange(), () => mods.count);

// --- environment ----------------------------------------------------------------

interface Check {
  name: string;
  ok: () => boolean;
  why: MessageKey;
  /** Without it the game cannot run at all. */
  required: boolean;
}
/** Hard requirements, shown only when missing. */
const BLOCKERS: [() => boolean, MessageKey][] = [
  [() => crossOriginIsolated, 'blockedIsolation'],
  [() => typeof SharedArrayBuffer !== 'undefined', 'blockedSharedMemory'],
  // JSPI: the game's threads suspend to present frames (src/jvm/platform.ts).
  [() => typeof (WebAssembly as { Suspending?: unknown }).Suspending === 'function', 'blockedJspi'],
];
/** Listed under "System check". */
const CHECKS: Check[] = [
  { name: 'OPFS', ok: () => typeof navigator.storage?.getDirectory === 'function', why: 'whyOpfs', required: true },
  { name: 'WebGL2', ok: () => typeof WebGL2RenderingContext !== 'undefined', why: 'whyWebgl', required: true },
  { name: 'WebAudio', ok: () => typeof AudioContext !== 'undefined', why: 'whyAudio', required: false },
];

const blockers = () => BLOCKERS.filter(([ok]) => !ok()).map(([, key]) => key);
const missingFeatures = () => CHECKS.filter((check) => !check.ok());

function renderEnvironment() {
  const blocked = blockers();
  ui.blocking.replaceChildren(...blocked.map((key) => Object.assign(document.createElement('p'), { textContent: tr(key) })));
  ui.blocking.hidden = blocked.length === 0;
  const missing = missingFeatures();
  ui.featureWarning.replaceChildren(
    ...missing.map((check) => Object.assign(document.createElement('p'), { textContent: tr('featureMissing', check.name, tr(check.why)) })),
  );
  ui.featureWarning.hidden = missing.length === 0;
  ui.env.replaceChildren(
    ...CHECKS.map((check) => {
      const ok = check.ok();
      const item = document.createElement('li');
      const mark = Object.assign(document.createElement('span'), { className: ok ? 'ok' : 'missing', textContent: ok ? '✓' : '✗' });
      item.append(mark, ` ${check.name} — ${tr(check.why)}`);
      return item;
    }),
  );
  ui.systemSummary.textContent = tr(missing.length ? 'systemCheckProblems' : 'systemCheckOk');
  if (missing.length) $<HTMLDetailsElement>('system').open = true;
}

function environmentProblem(): Text | null {
  const blocker = blockers()[0];
  if (blocker) return { key: blocker };
  const missing = missingFeatures().filter((check) => check.required);
  return missing.length ? { key: 'browserMissing', args: [missing.map((check) => check.name).join(', ')] } : null;
}

// --- versions -------------------------------------------------------------------

async function loadManifest() {
  setStatus({ key: 'loadingVersions' });
  try {
    const response = await fetch(VERSION_MANIFEST_URL);
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    manifest = (await response.json()) as VersionManifest;
    ui.snapshots.checked = prefs.get('snapshots') === '1';
    const tested = TESTED_VERSIONS.find((id) => manifest!.versions.some((v) => v.id === id));
    fillVersions(prefs.get('version') ?? tested ?? manifest.latest.release);
    setStatus(null);
  } catch (error) {
    setStatus({ key: 'manifestFailed', args: [translate(errorText(error))] }, true);
  }
}

const COMPAT_MARK: Record<Compat['level'], MessageKey | null> = { tested: 'markTested', untested: null, unsupported: 'markUnsupported' };

function fillVersions(selected: string) {
  if (!manifest) return;
  const { latest } = manifest;
  const versions = manifest.versions.filter(
    (v) => v.type === 'release' || (ui.snapshots.checked && v.type === 'snapshot') || v.id === selected,
  );
  ui.version.replaceChildren(
    ...versions.map((v) => {
      const tag = v.id === latest.release ? t.latestRelease : v.id === latest.snapshot ? t.latestSnapshot : '';
      const mark = COMPAT_MARK[manifestCompat(v)];
      return new Option(`${v.id}${tag}${mark ? tr(mark) : ''}`, v.id, false, v.id === selected);
    }),
  );
  void onVersionChange();
}

function selectedEntry(): VersionManifestEntry | undefined {
  return manifest?.versions.find((v) => v.id === ui.version.value);
}

function versionJson(entry: VersionManifestEntry): Promise<VersionJson> {
  let pending = versionJsons.get(entry.id);
  if (!pending) {
    pending = readJson<VersionJson>(layout.versionJson(entry.id))
      .catch(() => null)
      .then(async (cached) => {
        if (cached) return cached;
        const response = await fetch(entry.url);
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return (await response.json()) as VersionJson;
      });
    versionJsons.set(entry.id, pending);
    pending.catch(() => versionJsons.delete(entry.id));
  }
  return pending;
}

async function onVersionChange() {
  const entry = selectedEntry();
  compat = null;
  compatVersion = null;
  compatError = null;
  renderCompat();
  if (entry) loader.setGameVersion(entry.id);
  mods.setContext(entry?.id ?? '', loader.fabric);
  updatePlayButton();
  if (!entry) return;
  try {
    const version = await versionJson(entry);
    if (entry.id !== ui.version.value) return;
    compat = versionCompat(version);
    compatVersion = version;
    renderCompat();
    updatePlayButton();
    await refreshPlan();
  } catch (error) {
    compatError = { key: 'versionJsonFailed', args: [translate(errorText(error))] };
    renderCompat();
  }
  updatePlayButton();
}

function onLoaderChange() {
  mods.setContext(ui.version.value, loader.fabric);
  updatePlayButton();
  void refreshPlan();
}

/** The installed version JSON, merged with the Fabric profile when launching through Fabric. */
async function installedVersion(id: string, fabricLoader: string | undefined): Promise<VersionJson | null> {
  const vanilla = await readJson<VersionJson>(layout.versionJson(id));
  if (!vanilla || !fabricLoader) return vanilla;
  const profile = await readJson<FabricProfile>(layout.versionJson(fabricProfileId(id, fabricLoader)));
  return profile && mergeProfile(vanilla, profile);
}

/** The launch plan of what is installed for the selected version and loader. */
async function refreshPlan() {
  const id = ui.version.value;
  const fabricLoader = loader.fabricLoader;
  let next: LaunchPlan | null = null;
  try {
    const installed = id ? await installedVersion(id, fabricLoader) : null;
    next = installed && makePlan(installed);
  } catch {
    // Not (completely) installed yet.
  }
  if (id !== ui.version.value || fabricLoader !== loader.fabricLoader) return;
  plan = next;
  renderPlan();
}

function renderCompat() {
  const id = ui.version.value;
  if (compatError) {
    ui.compat.textContent = translate(compatError);
  } else if (!compat || !compatVersion) {
    ui.compat.textContent = id ? tr('checkingCompat') : '';
  } else {
    const mark = compat.level === 'tested' ? '✓ ' : compat.level === 'unsupported' ? '✗ ' : '';
    const java = `Java ${compatVersion.javaVersion?.majorVersion ?? 8} (${compatVersion.javaVersion?.component ?? 'jre-legacy'})`;
    ui.compat.textContent = `${mark}${id}: ${translate(compat.reason)}.${compat.level === 'unsupported' ? '' : tr('compatDownload', java)}`;
  }
  ui.compat.classList.toggle('error', compatError !== null || compat?.level === 'unsupported');
}

// --- play -----------------------------------------------------------------------

function playProblem(): Text | null {
  const environment = environmentProblem();
  if (environment) return environment;
  if (!manifest) return { key: 'noManifest' };
  if (compat?.level === 'unsupported') return { key: 'versionUnsupported', args: [ui.version.value] };
  const loaderProblem = loader.problem();
  if (loaderProblem) return loaderProblem;
  const accountProblem = account.problem();
  if (accountProblem) return { key: accountProblem };
  return ui.eula.checked ? null : { key: 'acceptEula' };
}

function updatePlayButton() {
  const problem = playProblem();
  ui.play.disabled = busy || problem !== null || compat === null;
  ui.verify.disabled = busy || !selectedEntry() || loader.problem() !== null;
  loader.disabled = busy;
  const hint = busy ? null : (problem ?? (compat?.level === 'untested' ? { key: 'untestedWarning' as const } : null));
  ui.playHint.textContent = hint ? translate(hint) : '';
}

function setStatus(text: Text | null, error = false) {
  status = text && { text, error };
  renderStatus();
}

function renderStatus() {
  ui.status.textContent = status ? translate(status.text) : '';
  ui.status.classList.toggle('error', status?.error ?? false);
}

function launchOptions(): LaunchOptions {
  const current = account.current;
  return {
    username: current?.profile?.name ?? 'Player',
    uuid: current?.profile ? dashedUuid(current.profile.id) : '00000000-0000-0000-0000-000000000000',
    accessToken: current?.minecraft.accessToken ?? '0',
    xuid: current?.xuid ?? '0',
    userType: 'msa',
    width: Math.max(854, Math.round(innerWidth)),
    height: Math.max(480, Math.round(innerHeight)),
  };
}

function makePlan(version: VersionJson): LaunchPlan {
  return buildLaunchPlan(version, launchOptions());
}

async function install(entry: VersionManifestEntry, full: boolean): Promise<boolean> {
  installer ??= new Installer();
  ui.cancel.hidden = false;
  ui.failures.hidden = true;
  ui.progress.hidden = false;
  showProgress({ totalFiles: 0, doneFiles: 0, skippedFiles: 0, totalBytes: 0, doneBytes: 0 });
  await requestPersistence().catch(() => false);
  const result = await installer.install(entry, full, loader.fabricLoader, onInstallEvent);
  ui.cancel.hidden = true;
  void showStorage();
  return result.type === 'done';
}

function onInstallEvent(event: InstallEvent) {
  switch (event.type) {
    case 'status':
      setStatus(event.text);
      break;
    case 'progress':
      showProgress(event);
      break;
    case 'done':
      showProgress(event);
      setStatus({ key: 'installDone', args: [event.versionId, event.runtime, event.doneFiles - event.skippedFiles, event.skippedFiles] });
      break;
    case 'error':
      setStatus(event.text, true);
      if (event.failures.length) {
        ui.failuresList.textContent = event.failures.join('\n');
        ui.failures.hidden = false;
      }
      break;
  }
}

async function verify() {
  const entry = selectedEntry();
  if (!entry || busy) return;
  busy = true;
  updatePlayButton();
  try {
    if (await install(entry, true)) await onVersionChange();
  } finally {
    busy = false;
    updatePlayButton();
  }
}

async function play() {
  const entry = selectedEntry();
  if (!entry || busy || playProblem()) return;
  busy = true;
  updatePlayButton();
  // While the game runs the launcher stays busy; "Stop" reloads the page.
  let started = false;
  try {
    setStatus({ key: 'checkingSignIn' });
    await account.ensureFresh();
    const fabricLoader = loader.fabricLoader;
    if (!(await install(entry, false))) return;
    const version = await installedVersion(entry.id, fabricLoader);
    if (!version) throw new Error(t.versionJsonMissing);
    plan = makePlan(version);
    renderPlan();
    await writeTextIfMissing('options.txt', DEFAULT_OPTIONS);
    await runGame(vmProcess(plan, { heapMb: Number(ui.memory.value), netBridge: multiplayer.address }), {
      onStatus: (message) => setStatus({ key: 'text', args: [message] }),
      onExit: (code) => setStatus({ key: 'gameExited', args: [code] }, code !== 0),
    });
    started = true;
  } catch (error) {
    setStatus(errorText(error), true);
  } finally {
    if (!started) {
      document.body.classList.remove('playing');
      $('game').hidden = true;
      busy = false;
      updatePlayButton();
    }
  }
}

function renderPlan() {
  ui.plan.hidden = plan === null;
  if (!plan) return;
  const nativeModules = [...new Set(plan.hostNatives.map((name) => parseMaven(name).artifact))].sort();
  const rows: [string, string][] = [
    [t.planVersion, plan.versionId],
    [t.planJava, t.planJavaValue(plan.javaMajorVersion, plan.javaComponent)],
    [t.planMainClass, plan.mainClass],
    [t.planClasspath, t.planClasspathValue(plan.classpath.length)],
    [t.planNatives, nativeModules.join(', ') || '—'],
  ];
  if (plan.unresolved.length) rows.push([t.planUnresolved, plan.unresolved.join(', ')]);
  ui.planSummary.replaceChildren(
    ...rows.flatMap(([term, value]) => [
      Object.assign(document.createElement('dt'), { textContent: term }),
      Object.assign(document.createElement('dd'), { textContent: value }),
    ]),
  );
  ui.planArgv.textContent = vmProcess(plan, { heapMb: Number(ui.memory.value), netBridge: multiplayer.address })
    .args.map((arg, i, args) => (args[i - 1] === '--accessToken' ? t.hidden : arg))
    .map((arg) => (arg.includes(':/') ? arg.split(':').join(':\n    ') : arg))
    .join('\n  ');
}

function showProgress(p: InstallProgress) {
  progress = p;
  ui.progressFill.style.width = `${p.totalBytes ? Math.min(100, (p.doneBytes / p.totalBytes) * 100) : 0}%`;
  ui.progressText.textContent =
    t.progress(p.doneFiles, p.totalFiles, formatBytes(p.doneBytes), formatBytes(p.totalBytes)) +
    (p.skippedFiles ? t.progressPresent(p.skippedFiles) : '');
}

// --- storage --------------------------------------------------------------------

async function showStorage() {
  try {
    storage = await storageInfo();
  } catch (error) {
    storage = { key: 'storageUnavailable', args: [translate(errorText(error))] };
  }
  renderStorage();
}

function renderStorage() {
  if (!storage) return;
  ui.storage.textContent =
    'key' in storage
      ? translate(storage)
      : t.storageUsage(formatBytes(storage.gameBytes), formatBytes(storage.quota)) + (storage.persisted ? t.storagePersistent : t.storageEvictable);
}

async function clear(all: boolean) {
  if (busy) return;
  if (!confirm(all ? t.confirmClearAll : t.confirmClearDownloads)) return;
  await (all ? clearGameDir() : clearDownloads());
  versionJsons.clear();
  await mods.refresh();
  setStatus({ key: all ? 'clearedAll' : 'clearedDownloads' });
  await onVersionChange();
  await showStorage();
}

// --- language ---------------------------------------------------------------------

function renderLanguage() {
  document.title = PRODUCT_NAME;
  $('product-name').textContent = PRODUCT_NAME;
  $('disclaimer').textContent = DISCLAIMER;
  ui.disclaimerLocal.textContent = t.disclaimerLocal ?? '';
  ui.disclaimerLocal.hidden = !t.disclaimerLocal;
  ui.lang.value = lang;
  renderEnvironment();
  if (manifest) fillVersions(ui.version.value);
  renderCompat();
  renderStatus();
  renderPlan();
  renderStorage();
  if (progress) showProgress(progress);
  updatePlayButton();
}

// --- wiring -------------------------------------------------------------------------

ui.lang.replaceChildren(...Object.entries(LANGUAGES).map(([code, { name }]) => new Option(name, code)));
ui.lang.addEventListener('change', () => setLang(ui.lang.value as Lang));
onLangChange(renderLanguage);

ui.memory.value = prefs.get('memory') ?? '1536';
ui.eula.checked = prefs.get('eula') === '1';
ui.version.addEventListener('change', () => {
  prefs.set('version', ui.version.value);
  void onVersionChange();
});
ui.snapshots.addEventListener('change', () => {
  prefs.set('snapshots', ui.snapshots.checked ? '1' : '0');
  fillVersions(ui.version.value);
});
ui.memory.addEventListener('change', () => {
  prefs.set('memory', ui.memory.value);
  renderPlan();
});
ui.eula.addEventListener('change', () => {
  prefs.set('eula', ui.eula.checked ? '1' : '0');
  updatePlayButton();
});
ui.play.addEventListener('click', () => void play());
ui.verify.addEventListener('click', () => void verify());
ui.cancel.addEventListener('click', () => installer?.cancel());
ui.clear.addEventListener('click', () => void clear(false));
ui.clearAll.addEventListener('click', () => void clear(true));

setLang(lang, false);
void showStorage();
void loadManifest();
