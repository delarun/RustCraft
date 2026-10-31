// Copies a pack (a directory tree in one response: dev-fs `?pack`, see
// vite.config.ts) into an OPFS directory, so a program reads its files from
// the browser's storage instead of asking a server for each. Per file: a u32
// path length, the UTF-8 path, a u32 size, the bytes (little-endian).

import type { PackMessage, PackRequest } from './pack';

declare const self: DedicatedWorkerGlobalScope;

interface SyncAccess {
  write(buffer: Uint8Array, options?: { at?: number }): number;
  truncate(size: number): void;
  close(): void;
}
type SyncCapable = FileSystemFileHandle & { createSyncAccessHandle(): Promise<SyncAccess> };

self.onmessage = (event: MessageEvent<PackRequest>) => {
  unpack(event.data).catch((e) => self.postMessage({ type: 'error', message: e instanceof Error ? e.message : String(e) } satisfies PackMessage));
};

async function unpack({ url, root }: PackRequest) {
  let top = await navigator.storage.getDirectory();
  for (const part of root.split('/').filter(Boolean)) top = await top.getDirectoryHandle(part, { create: true });
  const dirs = new Map<string, Promise<FileSystemDirectoryHandle>>([['', Promise.resolve(top)]]);
  const directory = (path: string): Promise<FileSystemDirectoryHandle> => {
    let dir = dirs.get(path);
    if (!dir) {
      const cut = path.lastIndexOf('/');
      dir = directory(cut < 0 ? '' : path.slice(0, cut)).then((d) => d.getDirectoryHandle(path.slice(cut + 1), { create: true }));
      dirs.set(path, dir);
    }
    return dir;
  };

  const response = await fetch(url);
  if (!response.ok || !response.body) throw new Error(`${url}: HTTP ${response.status}`);
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  // Bytes received and not yet used.
  let buffer = new Uint8Array(0);
  let done = false;
  const need = async (n: number) => {
    while (buffer.length < n && !done) {
      const chunk = await reader.read();
      if (chunk.done) {
        done = true;
        break;
      }
      const joined = new Uint8Array(buffer.length + chunk.value.length);
      joined.set(buffer);
      joined.set(chunk.value, buffer.length);
      buffer = joined;
    }
    return buffer.length >= n;
  };
  const take = (n: number) => {
    const out = buffer.subarray(0, n);
    buffer = buffer.subarray(n);
    return out;
  };
  const u32 = () => new DataView(take(4).slice().buffer).getUint32(0, true);

  let files = 0;
  let bytes = 0;
  let reported = performance.now();
  while (await need(4)) {
    const length = u32();
    await need(length + 4);
    const path = decoder.decode(take(length));
    let size = u32();
    const cut = path.lastIndexOf('/');
    const dir = await directory(cut < 0 ? '' : path.slice(0, cut));
    const file = await (dir.getFileHandle(path.slice(cut + 1), { create: true }) as Promise<SyncCapable>);
    const access = await file.createSyncAccessHandle();
    access.truncate(0);
    let at = 0;
    // Written as it arrives: a file may be bigger than what is buffered.
    while (size > 0) {
      if (!(await need(1))) throw new Error(`${url}: ends inside ${path}`);
      const part = take(Math.min(size, buffer.length));
      access.write(part, { at });
      at += part.length;
      size -= part.length;
    }
    access.close();
    files++;
    bytes += at;
    if (performance.now() - reported > 500) {
      reported = performance.now();
      self.postMessage({ type: 'progress', files, bytes } satisfies PackMessage);
    }
  }
  self.postMessage({ type: 'done', files, bytes } satisfies PackMessage);
}
