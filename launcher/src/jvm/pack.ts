// Keeps an OPFS directory a copy of a server's directory (a pack, see
// pack.worker.ts): copied again only when the server's tag for it changes.

/** What pack.worker.ts is asked to do. */
export interface PackRequest {
  url: string;
  /** The OPFS directory (slash-separated) that receives the files. */
  root: string;
}

/** What pack.worker.ts reports. */
export type PackMessage =
  | { type: 'progress'; files: number; bytes: number }
  | { type: 'done'; files: number; bytes: number }
  | { type: 'error'; message: string };

export interface Preload {
  /** The directory on a dev-fs style server (`?stat`, `?pack`); with `tag`, a pack file. */
  url: string;
  /** The OPFS directory (slash-separated) that holds the copy. */
  root: string;
  /** The pack file's version (e.g. its digest): copied again when it changes. */
  tag?: string;
}

const TAG_FILE = '.pack-tag';

async function directory(root: string, create: boolean): Promise<FileSystemDirectoryHandle | null> {
  let dir = await navigator.storage.getDirectory();
  try {
    for (const part of root.split('/').filter(Boolean)) dir = await dir.getDirectoryHandle(part, { create });
    return dir;
  } catch {
    return null;
  }
}

/** Copies `url` into `root` unless the copy there is current; reports progress lines. */
export async function ensurePack({ url, root, tag: packTag }: Preload, report: (line: string) => void): Promise<void> {
  let tag = packTag;
  if (tag === undefined) {
    // The directory's modification time and its listing: a file added,
    // removed or replaced changes one or the other.
    const [stat, list] = await Promise.all([fetch(`${url}?stat`), fetch(`${url}?list`)]);
    if (!stat.ok) throw new Error(`${url}: HTTP ${stat.status}`);
    tag = `${JSON.stringify(await stat.json())} ${(await list.text()).length}`;
  }
  const existing = await directory(root, false);
  const old = await existing
    ?.getFileHandle(TAG_FILE)
    .then((f) => f.getFile())
    .then((f) => f.text())
    .catch(() => null);
  if (old === tag) return;

  if (existing) {
    report(`${root}: removing the old copy`);
    const cut = root.lastIndexOf('/');
    const parent = await directory(cut < 0 ? '' : root.slice(0, cut), true);
    await parent!.removeEntry(root.slice(cut + 1), { recursive: true });
  }
  report(`${root}: copying ${url} into the browser's storage`);
  const started = performance.now();
  await new Promise<void>((resolve, reject) => {
    const worker = new Worker(new URL('./pack.worker.ts', import.meta.url), { type: 'module', name: 'pack' });
    worker.onmessage = (event: MessageEvent<PackMessage>) => {
      const m = event.data;
      if (m.type === 'error') {
        worker.terminate();
        reject(new Error(m.message));
        return;
      }
      report(`${root}: ${m.files} files, ${(m.bytes / 2 ** 20).toFixed(0)} MiB`);
      if (m.type === 'done') {
        worker.terminate();
        resolve();
      }
    };
    worker.onerror = (event) => reject(new Error(event.message));
    worker.postMessage({ url: new URL(packTag === undefined ? `${url}?pack` : url, location.href).href, root } satisfies PackRequest);
  });
  const dir = await directory(root, true);
  const writable = await (await dir!.getFileHandle(TAG_FILE, { create: true })).createWritable();
  await writable.write(tag);
  await writable.close();
  report(`${root}: copied in ${((performance.now() - started) / 1000).toFixed(1)} s`);
}
