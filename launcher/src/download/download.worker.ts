// Installs a game version into OPFS: version JSON, asset index, client jar,
// libraries, logging config, the assets needed at startup (the rest are
// fetched on first use, see src/jvm/opfs.ts) and Mojang's Java runtime for the
// version, each verified against Mojang's hashes.
// Runs in a worker because synchronous OPFS access handles are worker-only and
// hashing hundreds of megabytes should not block the UI.

import { collectDownloads, isEagerAsset, type DownloadJob } from '../mojang/downloads';
import { layout, OPFS_ROOT } from '../mojang/layout';
import { JAVA_RUNTIMES_URL, mirrorUrl } from '../mojang/mirror';
import { BROWSER_PLATFORM } from '../mojang/rules';
import { runtimeDownloads, selectRuntime, type RuntimeIndex, type RuntimeManifest } from '../mojang/runtime';
import type { AssetIndex, VersionJson, VersionManifestEntry } from '../mojang/types';
import { errorText } from '../i18n/error';
import type { InstallEvent, InstallProgress, InstallRequest } from './protocol';

declare const self: DedicatedWorkerGlobalScope;

const CONCURRENCY = 8;
const MAX_ATTEMPTS = 4;
const PROGRESS_INTERVAL_MS = 100;

let abort: AbortController | null = null;

self.onmessage = (event: MessageEvent<InstallRequest>) => {
  const request = event.data;
  if (request.type === 'cancel') {
    abort?.abort();
    return;
  }
  if (abort) {
    post({ type: 'error', text: { key: 'installBusy' }, failures: [] });
    return;
  }
  abort = new AbortController();
  install(request.entry, request.full, abort.signal)
    .catch((error: unknown) => post({ type: 'error', text: errorText(error), failures: [] }))
    .finally(() => (abort = null));
};

function post(event: InstallEvent) {
  self.postMessage(event);
}

async function install(entry: VersionManifestEntry, full: boolean, signal: AbortSignal) {
  post({ type: 'status', text: { key: 'installMetadata', args: [entry.id] } });
  const version = parseJson<VersionJson>(
    await fetchMetadata(entry.url, entry.sha1, layout.versionJson(entry.id), signal),
  );
  const index = version.assetIndex;
  const assets = parseJson<AssetIndex>(
    await fetchMetadata(index.url, index.sha1, layout.assetIndex(index.id), signal),
  );

  const runtime = version.javaVersion?.component ?? 'jre-legacy';
  post({ type: 'status', text: { key: 'installRuntime', args: [runtime] } });
  const runtimes = parseJson<RuntimeIndex>(await fetchMetadata(JAVA_RUNTIMES_URL, null, 'runtime/all.json', signal));
  const runtimeEntry = selectRuntime(runtimes, runtime);
  const runtimeManifest = parseJson<RuntimeManifest>(
    await fetchMetadata(runtimeEntry.manifest.url, runtimeEntry.manifest.sha1, layout.runtimeManifest(runtime), signal),
  );

  const jobs = [
    ...collectDownloads(version, assets, BROWSER_PLATFORM, isEagerAsset),
    ...runtimeDownloads(runtime, runtimeManifest),
  ];
  const progress: InstallProgress = {
    totalFiles: jobs.length,
    doneFiles: 0,
    skippedFiles: 0,
    totalBytes: jobs.reduce((sum, job) => sum + job.size, 0),
    doneBytes: 0,
  };
  post({ type: 'status', text: { key: 'installChecking', args: [full, jobs.length] } });

  let lastReport = 0;
  const report = (force = false) => {
    const now = performance.now();
    if (force || now - lastReport >= PROGRESS_INTERVAL_MS) {
      lastReport = now;
      post({ type: 'progress', ...progress });
    }
  };

  const failures: string[] = [];
  let next = 0;
  const runWorker = async () => {
    while (next < jobs.length && !signal.aborted) {
      const job = jobs[next++]!;
      try {
        let streamed = 0;
        if (await isInstalled(job, full)) {
          progress.skippedFiles++;
        } else {
          await downloadJob(job, signal, (bytes) => {
            streamed += bytes;
            progress.doneBytes += bytes;
            report();
          });
        }
        progress.doneBytes += job.size - streamed;
        progress.doneFiles++;
      } catch (error) {
        if (signal.aborted) return;
        failures.push(`${job.path}: ${describe(error)}`);
      }
      report();
    }
  };
  await Promise.all(Array.from({ length: CONCURRENCY }, runWorker));
  report(true);

  if (signal.aborted) {
    post({ type: 'error', text: { key: 'installCancelled' }, failures: [] });
  } else if (failures.length > 0) {
    post({ type: 'error', text: { key: 'installFailed', args: [failures.length] }, failures });
  } else {
    post({ type: 'done', versionId: version.id, runtime, ...progress });
  }
}

/** Metadata is always refetched when online, with the cached copy as offline fallback. */
async function fetchMetadata(url: string, sha1: string | null, path: string, signal: AbortSignal): Promise<Uint8Array> {
  try {
    const data = await fetchVerified(url, sha1, signal);
    await writeFile(path, data);
    return data;
  } catch (error) {
    if (signal.aborted) throw error;
    const cached = await readFile(path);
    if (cached && (sha1 === null || (await sha1Hex(cached)) === sha1)) return cached;
    throw error;
  }
}

async function isInstalled(job: DownloadJob, full: boolean): Promise<boolean> {
  if (job.verify === 'size' || !full) return (await fileSize(job.path)) === job.size;
  const data = await readFile(job.path);
  return data !== null && data.byteLength === job.size && (await sha1Hex(data)) === job.sha1;
}

async function downloadJob(job: DownloadJob, signal: AbortSignal, onBytes: (bytes: number) => void) {
  const data = await fetchVerified(job.url, job.sha1, signal, onBytes);
  if (data.byteLength !== job.size) throw new Error(`size ${data.byteLength}, expected ${job.size}`);
  await writeFile(job.path, data);
}

/** Downloads and checks the SHA-1 (null: no hash published); reports bytes as they arrive. */
async function fetchVerified(
  url: string,
  sha1: string | null,
  signal: AbortSignal,
  onBytes: (bytes: number) => void = () => {},
): Promise<Uint8Array> {
  let lastError: unknown;
  for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
    if (attempt > 0) await sleep(500 * 2 ** (attempt - 1), signal);
    let received = 0;
    try {
      const response = await fetch(mirrorUrl(url), { signal });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = await readBody(response, (bytes) => {
        received += bytes;
        onBytes(bytes);
      });
      const actual = await sha1Hex(data);
      if (sha1 !== null && actual !== sha1) throw new Error(`SHA-1 mismatch: expected ${sha1}, got ${actual}`);
      return data;
    } catch (error) {
      onBytes(-received);
      if (signal.aborted) throw error;
      lastError = error;
    }
  }
  throw lastError;
}

async function readBody(response: Response, onBytes: (bytes: number) => void): Promise<Uint8Array> {
  const length = Number(response.headers.get('Content-Length')) || 0;
  if (!response.body) return new Uint8Array(await response.arrayBuffer());
  const chunks: Uint8Array[] = [];
  let total = 0;
  const reader = response.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    total += value.length;
    onBytes(value.length);
  }
  if (chunks.length === 1 && (!length || length === total)) return chunks[0]!;
  const data = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    data.set(chunk, at);
    at += chunk.length;
  }
  return data;
}

// --- OPFS ---------------------------------------------------------------------

const directories = new Map<string, Promise<FileSystemDirectoryHandle>>();

function directory(path: string): Promise<FileSystemDirectoryHandle> {
  let handle = directories.get(path);
  if (!handle) {
    if (path === '') {
      handle = navigator.storage.getDirectory().then((root) => root.getDirectoryHandle(OPFS_ROOT, { create: true }));
    } else {
      const slash = path.lastIndexOf('/');
      handle = directory(slash < 0 ? '' : path.slice(0, slash)).then((parent) =>
        parent.getDirectoryHandle(path.slice(slash + 1), { create: true }),
      );
    }
    directories.set(path, handle);
    // Don't cache failures (e.g. quota errors) forever.
    handle.catch(() => directories.delete(path));
  }
  return handle;
}

async function fileHandle(path: string, create: boolean): Promise<FileSystemFileHandle | null> {
  const slash = path.lastIndexOf('/');
  const dir = await directory(slash < 0 ? '' : path.slice(0, slash));
  try {
    return await dir.getFileHandle(path.slice(slash + 1), { create });
  } catch (error) {
    if (error instanceof DOMException && error.name === 'NotFoundError') return null;
    throw error;
  }
}

async function fileSize(path: string): Promise<number | null> {
  const handle = await fileHandle(path, false);
  return handle ? (await handle.getFile()).size : null;
}

async function readFile(path: string): Promise<Uint8Array | null> {
  const handle = await fileHandle(path, false);
  return handle ? new Uint8Array(await (await handle.getFile()).arrayBuffer()) : null;
}

async function writeFile(path: string, data: Uint8Array) {
  const handle = (await fileHandle(path, true))!;
  const access = await handle.createSyncAccessHandle();
  try {
    access.truncate(0);
    access.write(data, { at: 0 });
    access.flush();
  } finally {
    access.close();
  }
}

// --- helpers ------------------------------------------------------------------

async function sha1Hex(data: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-1', data as Uint8Array<ArrayBuffer>));
  return Array.from(digest, (b) => b.toString(16).padStart(2, '0')).join('');
}

function parseJson<T>(data: Uint8Array): T {
  return JSON.parse(new TextDecoder().decode(data)) as T;
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener('abort', () => {
      clearTimeout(timer);
      reject(signal.reason);
    }, { once: true });
  });
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
