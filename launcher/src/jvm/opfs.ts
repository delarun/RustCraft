// A file system backend over the origin private file system (OPFS), for the
// I/O worker: the launcher's downloads (client, libraries, Java runtime,
// assets) and the game's own files (saves, options) live there and persist.
//
// Open files are FileSystemSyncAccessHandles (worker-only, synchronous reads
// and writes). A handle locks its file exclusively, so every descriptor of one
// path shares a single handle, closed with the last descriptor.
//
// Optionally a directory of content-addressed objects (assets/objects) is
// filled on demand: a missing `<xx>/<sha1>` that the asset index lists looks
// present, and opening it fetches it (verified against the hash) into OPFS.

import { basename, FsError, parent, type Backend, type Handle, type OpenFlags, type Stat } from './fs';
import type { LazyObjects } from './protocol';

const ACCES = 2;
const EXIST = 20;
const IO = 29;
const ISDIR = 31;
const NOENT = 44;
const NOTDIR = 54;
const NOTEMPTY = 55;
const NOTSUP = 58;
const ROFS = 69;

/** The worker-only parts of the OPFS API (the page's DOM typings lack them). */
interface SyncAccess {
  read(buffer: Uint8Array, options?: { at?: number }): number;
  write(buffer: Uint8Array, options?: { at?: number }): number;
  truncate(size: number): void;
  getSize(): number;
  flush(): void;
  close(): void;
}
type SyncCapable = FileSystemFileHandle & { createSyncAccessHandle(): Promise<SyncAccess> };
type Movable = FileSystemHandle & { move(parent: FileSystemDirectoryHandle, name: string): Promise<void> };

interface OpenFile {
  path: string;
  access: SyncAccess;
  refs: number;
  mtime: number;
}

const isDomError = (error: unknown, name: string) => error instanceof DOMException && error.name === name;

/** OPFS errors as WASI errnos. */
function errno(error: unknown): never {
  if (error instanceof FsError) throw error;
  if (isDomError(error, 'NotFoundError')) throw new FsError(NOENT);
  if (isDomError(error, 'TypeMismatchError')) throw new FsError(NOTDIR);
  if (isDomError(error, 'InvalidModificationError')) throw new FsError(NOTEMPTY);
  if (isDomError(error, 'NoModificationAllowedError')) throw new FsError(ACCES);
  if (isDomError(error, 'NotAllowedError') || isDomError(error, 'SecurityError')) throw new FsError(ACCES);
  if (error instanceof TypeError) throw new FsError(NOENT); // an invalid name
  throw error;
}

export class OpfsBackend implements Backend {
  readonly readonly: boolean;
  private lazy?: LazyObjects;
  private dirs = new Map<string, Promise<FileSystemDirectoryHandle>>();
  private files = new Map<string, OpenFile>();
  private lazyIndex?: Promise<{ objects: Map<string, number>; prefixes: Set<string> }>;
  private fetching = new Map<string, Promise<void>>();
  /** For reads into shared memory, which the access handle may refuse. */
  private scratch = new Uint8Array(1 << 16);

  constructor(
    private root: string,
    options: { readonly?: boolean; lazy?: LazyObjects } = {},
  ) {
    this.readonly = options.readonly ?? false;
    this.lazy = options.lazy;
  }

  // --- directories -----------------------------------------------------------

  private directory(path: string): Promise<FileSystemDirectoryHandle> {
    let handle = this.dirs.get(path);
    if (!handle) {
      if (path === '/') {
        handle = this.root
          .split('/')
          .filter(Boolean)
          .reduce<Promise<FileSystemDirectoryHandle>>(
            (dir, name) => dir.then((d) => d.getDirectoryHandle(name, { create: true })),
            navigator.storage.getDirectory(),
          );
      } else {
        handle = this.directory(parent(path)).then((dir) => dir.getDirectoryHandle(basename(path)));
      }
      this.dirs.set(path, handle);
      handle.catch(() => this.dirs.delete(path));
    }
    return handle.catch(errno);
  }

  /** Forgets cached handles of a directory tree that was removed or moved. */
  private forget(path: string) {
    for (const key of this.dirs.keys()) {
      if (key === path || key.startsWith(`${path}/`)) this.dirs.delete(key);
    }
  }

  private async entry(path: string): Promise<FileSystemFileHandle | FileSystemDirectoryHandle | null> {
    if (path === '/') return this.directory('/');
    let dir: FileSystemDirectoryHandle;
    try {
      dir = await this.directory(parent(path));
    } catch {
      return null;
    }
    const name = basename(path);
    try {
      return await dir.getFileHandle(name);
    } catch (error) {
      if (!isDomError(error, 'TypeMismatchError')) {
        if (isDomError(error, 'NotFoundError') || error instanceof TypeError) return null;
        errno(error);
      }
    }
    return this.directory(path);
  }

  // --- Backend ---------------------------------------------------------------

  async stat(path: string): Promise<Stat | null> {
    const open = this.files.get(path);
    if (open) return { kind: 'file', size: open.access.getSize(), mtime: open.mtime };
    const entry = await this.entry(path);
    if (entry?.kind === 'directory') return { kind: 'dir', size: 0, mtime: 0 };
    if (entry) {
      const file = await (entry as FileSystemFileHandle).getFile().catch(errno);
      return { kind: 'file', size: file.size, mtime: file.lastModified };
    }
    const lazy = await this.lazyLookup(path);
    if (lazy === 'dir') return { kind: 'dir', size: 0, mtime: 0 };
    return lazy === null ? null : { kind: 'file', size: lazy.size, mtime: 0 };
  }

  async list(path: string): Promise<string[]> {
    const names = new Set<string>();
    try {
      const dir = await this.directory(path);
      for await (const name of dir.keys()) names.add(name);
    } catch (error) {
      if (!(error instanceof FsError && error.errno === NOENT) || !(await this.lazyLookup(path))) throw error;
    }
    for (const name of await this.lazyList(path)) names.add(name);
    return [...names];
  }

  async open(path: string, flags: OpenFlags): Promise<Handle> {
    const writes = flags.write || flags.truncate;
    let file = this.files.get(path);
    if (file && flags.exclusive && flags.create) throw new FsError(EXIST);
    if (!file) {
      let entry = await this.entry(path);
      if (entry?.kind === 'directory') throw new FsError(ISDIR);
      if (entry && flags.exclusive && flags.create) throw new FsError(EXIST);
      if (!entry) {
        const lazy = await this.lazyLookup(path);
        if (lazy !== null && lazy !== 'dir' && !flags.truncate) {
          await this.fetchObject(lazy.hash, lazy.size);
          entry = await this.entry(path);
        }
      }
      if (!entry) {
        if (!flags.create) throw new FsError(NOENT);
        if (this.readonly) throw new FsError(ROFS);
        const dir = await this.directory(parent(path));
        entry = await dir.getFileHandle(basename(path), { create: true }).catch(errno);
      }
      if (this.readonly && writes) throw new FsError(ROFS);
      // Another descriptor may have opened it meanwhile.
      file = this.files.get(path);
      if (!file) {
        const access = await (entry as SyncCapable).createSyncAccessHandle().catch(errno);
        file = this.files.get(path);
        if (file) access.close();
        else {
          file = { path, access, refs: 0, mtime: Date.now() };
          this.files.set(path, file);
        }
      }
    } else if (this.readonly && writes) {
      throw new FsError(ROFS);
    }
    file.refs++;
    if (flags.truncate) {
      file.access.truncate(0);
      file.mtime = Date.now();
    }
    return this.handle(file);
  }

  private handle(file: OpenFile): Handle {
    let closed = false;
    const live = () => {
      if (closed || this.files.get(file.path) !== file) throw new FsError(IO);
      return file.access;
    };
    return {
      size: () => live().getSize(),
      read: (position, into) => {
        const access = live();
        if (!(into.buffer instanceof SharedArrayBuffer)) return access.read(into, { at: position });
        let total = 0;
        while (total < into.length) {
          if (this.scratch.length < Math.min(into.length, 1 << 22)) this.scratch = new Uint8Array(Math.min(into.length, 1 << 22));
          const chunk = this.scratch.subarray(0, Math.min(this.scratch.length, into.length - total));
          const n = access.read(chunk, { at: position + total });
          into.set(chunk.subarray(0, n), total);
          total += n;
          if (n < chunk.length) break;
        }
        return total;
      },
      write: (position, from) => {
        const n = live().write(from, { at: position });
        file.mtime = Date.now();
        return n;
      },
      truncate: (size) => {
        live().truncate(size);
        file.mtime = Date.now();
      },
      flush: () => live().flush(),
      close: () => {
        if (closed) return;
        closed = true;
        if (this.files.get(file.path) === file && --file.refs <= 0) this.release(file);
      },
    };
  }

  private release(file: OpenFile) {
    this.files.delete(file.path);
    try {
      file.access.flush();
    } finally {
      file.access.close();
    }
  }

  async mkdir(path: string): Promise<void> {
    if (this.readonly) throw new FsError(ROFS);
    const dir = await this.directory(parent(path));
    if (await this.entry(path)) throw new FsError(EXIST);
    await dir.getDirectoryHandle(basename(path), { create: true }).catch(errno);
  }

  async unlink(path: string): Promise<void> {
    if (this.readonly) throw new FsError(ROFS);
    const entry = await this.entry(path);
    if (!entry) throw new FsError(NOENT);
    if (entry.kind === 'directory') throw new FsError(ISDIR);
    // POSIX lets open files be removed; here their descriptors stop working.
    const open = this.files.get(path);
    if (open) this.release(open);
    await (await this.directory(parent(path))).removeEntry(basename(path)).catch(errno);
  }

  async rmdir(path: string): Promise<void> {
    if (this.readonly) throw new FsError(ROFS);
    if (path === '/') throw new FsError(ACCES);
    const entry = await this.entry(path);
    if (!entry) throw new FsError(NOENT);
    if (entry.kind !== 'directory') throw new FsError(NOTDIR);
    await (await this.directory(parent(path))).removeEntry(basename(path)).catch(errno);
    this.forget(path);
  }

  async rename(from: string, to: string): Promise<void> {
    if (this.readonly) throw new FsError(ROFS);
    if (from === to) return;
    const source = await this.entry(from);
    if (!source) throw new FsError(NOENT);
    const target = await this.entry(to);
    const targetDir = await this.directory(parent(to));
    if (target) {
      if (target.kind === 'directory') {
        if (source.kind !== 'directory') throw new FsError(ISDIR);
        // Only empty directories may be replaced.
        await targetDir.removeEntry(basename(to)).catch(errno);
        this.forget(to);
      } else {
        if (source.kind === 'directory') throw new FsError(NOTDIR);
        const open = this.files.get(to);
        if (open) this.release(open);
        await targetDir.removeEntry(basename(to)).catch(errno);
      }
    }
    // A locked file cannot move: close its handle and reopen it at the new path.
    const open = this.files.get(from);
    if (open) {
      open.access.flush();
      open.access.close();
      this.files.delete(from);
    }
    try {
      await (source as unknown as Movable).move(targetDir, basename(to));
    } catch (error) {
      if (open) open.access = await (source as unknown as SyncCapable).createSyncAccessHandle();
      if (open) this.files.set(from, open);
      if (error instanceof TypeError && !('move' in source)) throw new FsError(NOTSUP);
      errno(error);
    }
    if (source.kind === 'directory') {
      this.forget(from);
    } else if (open) {
      const moved = await targetDir.getFileHandle(basename(to)).catch(errno);
      open.access = await (moved as SyncCapable).createSyncAccessHandle().catch(errno);
      open.path = to;
      this.files.set(to, open);
    }
  }

  // --- objects fetched on first use -------------------------------------------

  /** A missing path inside the lazy directory: an indexed object, a fan-out directory, or nothing. */
  private async lazyLookup(path: string): Promise<{ hash: string; size: number } | 'dir' | null> {
    const lazy = this.lazy;
    if (!lazy || !(path === lazy.dir || path.startsWith(`${lazy.dir}/`))) return null;
    const rest = path.slice(lazy.dir.length).split('/').filter(Boolean);
    if (rest.length === 0) return 'dir';
    const { objects, prefixes } = await this.objectIndex();
    if (rest.length === 1) return prefixes.has(rest[0]!) ? 'dir' : null;
    const [prefix, hash] = rest as [string, string];
    if (rest.length !== 2 || !hash.startsWith(prefix)) return null;
    const size = objects.get(hash);
    return size === undefined ? null : { hash, size };
  }

  private async lazyList(path: string): Promise<string[]> {
    const lazy = this.lazy;
    if (!lazy || !(path === lazy.dir || path.startsWith(`${lazy.dir}/`))) return [];
    const rest = path.slice(lazy.dir.length).split('/').filter(Boolean);
    const { objects, prefixes } = await this.objectIndex();
    if (rest.length === 0) return [...prefixes];
    if (rest.length === 1) return [...objects.keys()].filter((h) => h.startsWith(rest[0]!));
    return [];
  }

  private objectIndex(): Promise<{ objects: Map<string, number>; prefixes: Set<string> }> {
    this.lazyIndex ??= (async () => {
      const objects = new Map<string, number>();
      const prefixes = new Set<string>();
      const entry = await this.entry(this.lazy!.index);
      if (entry?.kind !== 'file') return { objects, prefixes };
      const index = JSON.parse(await (await (entry as FileSystemFileHandle).getFile()).text()) as {
        objects?: Record<string, { hash: string; size: number }>;
      };
      for (const { hash, size } of Object.values(index.objects ?? {})) {
        objects.set(hash, size);
        prefixes.add(hash.slice(0, 2));
      }
      return { objects, prefixes };
    })();
    return this.lazyIndex;
  }

  private fetchObject(hash: string, size: number): Promise<void> {
    let pending = this.fetching.get(hash);
    if (!pending) {
      pending = (async () => {
        const lazy = this.lazy!;
        const data = await fetchVerified(`${lazy.url}/${hash.slice(0, 2)}/${hash}`, hash, size);
        let dir = await this.directory(parent(lazy.dir));
        for (const name of [basename(lazy.dir), hash.slice(0, 2)]) dir = await dir.getDirectoryHandle(name, { create: true });
        // Write under a temporary name so an interrupted download never looks complete.
        const partial = `${hash}.part`;
        const access = await ((await dir.getFileHandle(partial, { create: true })) as SyncCapable).createSyncAccessHandle();
        try {
          access.truncate(0);
          access.write(data, { at: 0 });
          access.flush();
        } finally {
          access.close();
        }
        const written = await dir.getFileHandle(partial);
        await (written as unknown as Movable).move(dir, hash);
      })();
      this.fetching.set(hash, pending);
      pending.finally(() => this.fetching.delete(hash)).catch(() => {});
    }
    return pending.catch((error: unknown) => {
      console.error(`opfs: could not fetch ${hash}:`, error);
      throw new FsError(IO);
    });
  }
}

async function fetchVerified(url: string, sha1: string, size: number): Promise<Uint8Array> {
  let lastError: unknown;
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      const response = await fetch(url);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = new Uint8Array(await response.arrayBuffer());
      if (data.length !== size) throw new Error(`size ${data.length}, expected ${size}`);
      const digest = new Uint8Array(await crypto.subtle.digest('SHA-1', data));
      const actual = Array.from(digest, (b) => b.toString(16).padStart(2, '0')).join('');
      if (actual !== sha1) throw new Error(`SHA-1 ${actual}, expected ${sha1}`);
      return data;
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, 500 * 2 ** attempt));
    }
  }
  throw lastError;
}
