// Main-thread access to the game directory in OPFS: reading metadata, small
// files (options.txt) and the player's mods. Game downloads are written by the
// download worker.

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
  await writeFile(path, text);
  return true;
}

/** Creates or replaces a file, creating its directories. */
export async function writeFile(path: string, data: string | Blob | Uint8Array<ArrayBuffer>) {
  let dir = await gameDir();
  const parts = path.split('/');
  const name = parts.pop()!;
  for (const part of parts) dir = await dir.getDirectoryHandle(part, { create: true });
  const writable = await (await dir.getFileHandle(name, { create: true })).createWritable();
  try {
    await writable.write(data);
    await writable.close();
  } catch (error) {
    await writable.abort().catch(() => {});
    throw error;
  }
}

export async function removeFile(path: string) {
  let dir = await gameDir();
  const parts = path.split('/');
  const name = parts.pop()!;
  try {
    for (const part of parts) dir = await dir.getDirectoryHandle(part);
    await dir.removeEntry(name);
  } catch (error) {
    if (!(error instanceof DOMException && error.name === 'NotFoundError')) throw error;
  }
}

/** The files directly in a directory (none if it does not exist). */
export async function listFiles(path: string): Promise<{ name: string; size: number }[]> {
  let dir = await gameDir();
  try {
    for (const part of path.split('/')) dir = await dir.getDirectoryHandle(part);
  } catch (error) {
    if (error instanceof DOMException && error.name === 'NotFoundError') return [];
    throw error;
  }
  const files: Promise<{ name: string; size: number }>[] = [];
  for await (const handle of dir.values()) {
    if (handle.kind === 'file') files.push((handle as FileSystemFileHandle).getFile().then((file) => ({ name: handle.name, size: file.size })));
  }
  return Promise.all(files);
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
