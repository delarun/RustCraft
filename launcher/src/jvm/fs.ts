// File system backends for the I/O worker. Metadata operations may be
// asynchronous; open files are read and written synchronously.

export interface Stat {
  kind: 'file' | 'dir';
  size: number;
  /** Milliseconds since the epoch. */
  mtime: number;
}

export interface OpenFlags {
  write: boolean;
  create: boolean;
  exclusive: boolean;
  truncate: boolean;
}

export interface Handle {
  size(): number;
  read(position: number, into: Uint8Array): number;
  write(position: number, from: Uint8Array): number;
  truncate(size: number): void;
  flush(): void;
  close(): void;
}

/** Errors carry a WASI errno. */
export class FsError extends Error {
  constructor(readonly errno: number) {
    super(`errno ${errno}`);
  }
}

export interface Backend {
  readonly readonly: boolean;
  stat(path: string): Promise<Stat | null>;
  list(path: string): Promise<string[]>;
  open(path: string, flags: OpenFlags): Promise<Handle>;
  mkdir(path: string): Promise<void>;
  unlink(path: string): Promise<void>;
  rmdir(path: string): Promise<void>;
  rename(from: string, to: string): Promise<void>;
}

const NOENT = 44;
const EXIST = 20;
const ISDIR = 31;
const NOTDIR = 54;
const NOTEMPTY = 55;
const ROFS = 69;

export const parent = (path: string) => path.slice(0, Math.max(path.lastIndexOf('/'), 0)) || '/';
export const basename = (path: string) => path.slice(path.lastIndexOf('/') + 1);

/** Read-only files over HTTP: `<url><path>?stat`, `?list` and the bytes. */
export class HttpBackend implements Backend {
  readonly readonly = true;
  private stats = new Map<string, Promise<Stat | null>>();
  private contents = new Map<string, Promise<Uint8Array>>();

  constructor(private url: string) {}

  stat(path: string): Promise<Stat | null> {
    let stat = this.stats.get(path);
    if (!stat) {
      stat = fetch(`${this.url}${encodePath(path)}?stat`).then((r) => (r.ok ? (r.json() as Promise<Stat>) : null));
      this.stats.set(path, stat);
    }
    return stat;
  }

  async list(path: string): Promise<string[]> {
    const response = await fetch(`${this.url}${encodePath(path)}?list`);
    if (!response.ok) throw new FsError(NOENT);
    return (await response.json()) as string[];
  }

  async open(path: string, flags: OpenFlags): Promise<Handle> {
    if (flags.write || flags.truncate) throw new FsError(ROFS);
    const stat = await this.stat(path);
    if (!stat) throw new FsError(NOENT);
    if (stat.kind === 'dir') throw new FsError(ISDIR);
    let content = this.contents.get(path);
    if (!content) {
      content = fetch(`${this.url}${encodePath(path)}`).then(async (r) => {
        if (!r.ok) throw new FsError(NOENT);
        return new Uint8Array(await r.arrayBuffer());
      });
      this.contents.set(path, content);
    }
    const data = await content;
    return {
      size: () => data.length,
      read: (position, into) => {
        const n = Math.max(0, Math.min(into.length, data.length - position));
        into.set(data.subarray(position, position + n));
        return n;
      },
      write: () => {
        throw new FsError(ROFS);
      },
      truncate: () => {
        throw new FsError(ROFS);
      },
      flush: () => {},
      close: () => {},
    };
  }

  mkdir(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
  unlink(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
  rmdir(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
  rename(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
}

const encodePath = (path: string) => path.split('/').map(encodeURIComponent).join('/');

interface MemNode {
  kind: 'file' | 'dir';
  data: Uint8Array;
  size: number;
  mtime: number;
  children: Map<string, MemNode>;
}

const memNode = (kind: 'file' | 'dir'): MemNode => ({
  kind,
  data: new Uint8Array(0),
  size: 0,
  mtime: Date.now(),
  children: new Map(),
});

/** A writable in-memory tree. */
export class MemoryBackend implements Backend {
  readonly readonly = false;
  private root = memNode('dir');

  private find(path: string): MemNode | null {
    let node: MemNode | undefined = this.root;
    for (const part of path.split('/').filter(Boolean)) {
      if (node.kind !== 'dir') return null;
      node = node.children.get(part);
      if (!node) return null;
    }
    return node;
  }

  private dir(path: string): MemNode {
    const node = this.find(path);
    if (!node) throw new FsError(NOENT);
    if (node.kind !== 'dir') throw new FsError(NOTDIR);
    return node;
  }

  async stat(path: string): Promise<Stat | null> {
    const node = this.find(path);
    return node && { kind: node.kind, size: node.size, mtime: node.mtime };
  }

  async list(path: string): Promise<string[]> {
    return [...this.dir(path).children.keys()];
  }

  async open(path: string, flags: OpenFlags): Promise<Handle> {
    let node = this.find(path);
    if (node && flags.exclusive) throw new FsError(EXIST);
    if (!node) {
      if (!flags.create) throw new FsError(NOENT);
      node = memNode('file');
      this.dir(parent(path)).children.set(basename(path), node);
    }
    if (node.kind === 'dir') {
      if (flags.write) throw new FsError(ISDIR);
    } else if (flags.truncate) {
      node.size = 0;
    }
    const file = node;
    const reserve = (size: number) => {
      if (size > file.data.length) {
        const grown = new Uint8Array(Math.max(size, file.data.length * 2, 4096));
        grown.set(file.data.subarray(0, file.size));
        file.data = grown;
      }
    };
    return {
      size: () => file.size,
      read: (position, into) => {
        const n = Math.max(0, Math.min(into.length, file.size - position));
        into.set(file.data.subarray(position, position + n));
        return n;
      },
      write: (position, from) => {
        reserve(position + from.length);
        if (position > file.size) file.data.fill(0, file.size, position);
        file.data.set(from, position);
        file.size = Math.max(file.size, position + from.length);
        file.mtime = Date.now();
        return from.length;
      },
      truncate: (size) => {
        reserve(size);
        if (size > file.size) file.data.fill(0, file.size, size);
        file.size = size;
        file.mtime = Date.now();
      },
      flush: () => {},
      close: () => {},
    };
  }

  async mkdir(path: string): Promise<void> {
    const dir = this.dir(parent(path));
    if (dir.children.has(basename(path))) throw new FsError(EXIST);
    dir.children.set(basename(path), memNode('dir'));
  }

  async unlink(path: string): Promise<void> {
    const node = this.find(path);
    if (!node) throw new FsError(NOENT);
    if (node.kind === 'dir') throw new FsError(ISDIR);
    this.dir(parent(path)).children.delete(basename(path));
  }

  async rmdir(path: string): Promise<void> {
    const node = this.dir(path);
    if (node.children.size) throw new FsError(NOTEMPTY);
    this.dir(parent(path)).children.delete(basename(path));
  }

  async rename(from: string, to: string): Promise<void> {
    const node = this.find(from);
    if (!node) throw new FsError(NOENT);
    const target = this.find(to);
    if (target?.kind === 'dir' && target.children.size) throw new FsError(NOTEMPTY);
    this.dir(parent(from)).children.delete(basename(from));
    this.dir(parent(to)).children.set(basename(to), node);
  }
}

/** A writable layer over a read-only backend: files are copied up when
 *  opened for writing; removed lower files are hidden. */
export class OverlayBackend implements Backend {
  readonly readonly = false;
  private upper = new MemoryBackend();
  private hidden = new Set<string>();

  constructor(private lower: Backend) {}

  async stat(path: string): Promise<Stat | null> {
    const upper = await this.upper.stat(path);
    if (upper || this.hidden.has(path)) return upper;
    return this.lower.stat(path);
  }

  async list(path: string): Promise<string[]> {
    const names = new Set(await this.upper.list(path).catch(() => [] as string[]));
    for (const name of await this.lower.list(path).catch(() => [] as string[])) {
      if (!this.hidden.has(path === '/' ? `/${name}` : `${path}/${name}`)) names.add(name);
    }
    return [...names];
  }

  /** Makes `path`'s parent directories exist in the upper layer. */
  private async parents(path: string) {
    const parts = path.split('/').filter(Boolean).slice(0, -1);
    let dir = '';
    for (const part of parts) {
      dir += `/${part}`;
      if (!(await this.upper.stat(dir))) await this.upper.mkdir(dir);
    }
  }

  async open(path: string, flags: OpenFlags): Promise<Handle> {
    if (await this.upper.stat(path)) return this.upper.open(path, flags);
    const lower = this.hidden.has(path) ? null : await this.lower.stat(path);
    if (!flags.write && !flags.truncate && lower) return this.lower.open(path, flags);
    await this.parents(path);
    if (lower?.kind === 'file' && !flags.truncate) {
      // Copy up.
      const source = await this.lower.open(path, { write: false, create: false, exclusive: false, truncate: false });
      const bytes = new Uint8Array(source.size());
      source.read(0, bytes);
      const target = await this.upper.open(path, { write: true, create: true, exclusive: false, truncate: true });
      target.write(0, bytes);
    }
    this.hidden.delete(path);
    return this.upper.open(path, { ...flags, create: true, exclusive: flags.exclusive && !lower });
  }

  async mkdir(path: string): Promise<void> {
    await this.parents(path);
    this.hidden.delete(path);
    return this.upper.mkdir(path);
  }

  async unlink(path: string): Promise<void> {
    if (await this.upper.stat(path)) await this.upper.unlink(path);
    this.hidden.add(path);
  }

  async rmdir(path: string): Promise<void> {
    if (await this.upper.stat(path)) await this.upper.rmdir(path);
    this.hidden.add(path);
  }

  async rename(from: string, to: string): Promise<void> {
    const stat = await this.stat(from);
    if (!stat) throw new FsError(NOENT);
    if (!(await this.upper.stat(from))) {
      // Copy the lower file up first.
      (await this.open(from, { write: true, create: false, exclusive: false, truncate: false })).close();
    }
    await this.parents(to);
    await this.upper.rename(from, to);
    this.hidden.add(from);
    this.hidden.delete(to);
  }
}

/** /dev: random, urandom (crypto.getRandomValues), null and zero. */
export class DeviceBackend implements Backend {
  readonly readonly = false;
  private static DEVICES = ['random', 'urandom', 'null', 'zero'];

  async stat(path: string): Promise<Stat | null> {
    if (path === '/') return { kind: 'dir', size: 0, mtime: 0 };
    return DeviceBackend.DEVICES.includes(path.slice(1)) ? { kind: 'file', size: 0, mtime: 0 } : null;
  }

  async list(): Promise<string[]> {
    return DeviceBackend.DEVICES;
  }

  async open(path: string): Promise<Handle> {
    const name = path.slice(1);
    if (!DeviceBackend.DEVICES.includes(name)) throw new FsError(NOENT);
    return {
      size: () => 0,
      read: (_position, into) => {
        if (name === 'null') return 0;
        if (name === 'zero') {
          into.fill(0);
          return into.length;
        }
        // getRandomValues takes no shared views, at most 64 KiB at a time.
        const chunk = new Uint8Array(Math.min(into.length, 65536));
        crypto.getRandomValues(chunk);
        into.set(chunk);
        return chunk.length;
      },
      write: (_position, from) => from.length,
      truncate: () => {},
      flush: () => {},
      close: () => {},
    };
  }

  mkdir(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
  unlink(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
  rmdir(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
  rename(): Promise<void> {
    return Promise.reject(new FsError(ROFS));
  }
}
