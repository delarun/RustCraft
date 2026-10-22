// Main-thread, read-only access to the game directory in OPFS. Writes happen in
// the download worker.

import { DOWNLOAD_DIRS, OPFS_ROOT } from '../mojang/layout';

async function gameDir(): Promise<FileSystemDirectoryHandle> {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(OPFS_ROOT, { create: true });
}

export async function readText(path: string): Promise<string | null> {
  let dir = await gameDir();
  const parts = path.split('/');
  const name = parts.pop()!;
  try {
    for (const part of parts) dir = await dir.getDirectoryHandle(part);
    const file = await (await dir.getFileHandle(name)).getFile();
    return await file.text();
  } catch (error) {
    if (error instanceof DOMException && error.name === 'NotFoundError') return null;
    throw error;
  }
}

/** Writes a small file at `path` unless it exists; returns whether it wrote. */
export async function writeTextIfMissing(path: string, text: string): Promise<boolean> {
  if ((await readText(path)) !== null) return false;
  let dir = await gameDir();
  const parts = path.split('/');
  const name = parts.pop()!;
  for (const part of parts) dir = await dir.getDirectoryHandle(part, { create: true });
  const writable = await (await dir.getFileHandle(name, { create: true })).createWritable();
  await writable.write(text);
  await writable.close();
  return true;
}

export async function readJson<T>(path: string): Promise<T | null> {
  const text = await readText(path);
  return text === null ? null : (JSON.parse(text) as T);
}

/** Removes downloaded files (versions, libraries, assets, Java runtimes); keeps saves and options. */
export async function clearDownloads() {
  const dir = await gameDir();
  for (const name of DOWNLOAD_DIRS) {
    await dir.removeEntry(name, { recursive: true }).catch((error: unknown) => {
      if (!(error instanceof DOMException && error.name === 'NotFoundError')) throw error;
    });
  }
}

/** Removes everything, the player's worlds included. */
export async function clearGameDir() {
  const root = await navigator.storage.getDirectory();
  await root.removeEntry(OPFS_ROOT, { recursive: true }).catch((error: unknown) => {
    if (!(error instanceof DOMException && error.name === 'NotFoundError')) throw error;
  });
}

export interface StorageInfo {
  gameBytes: number;
  quota: number;
  persisted: boolean;
}

export async function storageInfo(): Promise<StorageInfo> {
  // estimate().usage lags far behind OPFS writes in Chrome, so measure the files directly.
  const { quota = 0 } = await navigator.storage.estimate();
  return {
    gameBytes: await directorySize(await gameDir()),
    quota,
    persisted: await navigator.storage.persisted(),
  };
}

async function directorySize(dir: FileSystemDirectoryHandle): Promise<number> {
  const sizes: Promise<number>[] = [];
  for await (const handle of dir.values()) {
    sizes.push(
      handle.kind === 'file'
        ? (handle as FileSystemFileHandle).getFile().then((file) => file.size)
        : directorySize(handle as FileSystemDirectoryHandle),
    );
  }
  return (await Promise.all(sizes)).reduce((sum, size) => sum + size, 0);
}

/** Ask the browser not to evict our ~1 GB of game files under storage pressure. */
export function requestPersistence(): Promise<boolean> {
  return navigator.storage.persist();
}
