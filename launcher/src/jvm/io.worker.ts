// The JVM process's file system: serves the WASI file calls of every VM
// thread against one descriptor table and a mount table of backends. A thread
// writes its call into its slot of the control block, rings its port and
// blocks; the result goes into its memory and the slot.

import { DeviceBackend, FsError, HttpBackend, MemoryBackend, OverlayBackend, parent, type Backend, type Handle, type Stat } from './fs';
import { OpfsBackend } from './opfs';
import {
  ERRNO,
  FILETYPE,
  IO_CALLS,
  slotArg,
  slotInt,
  STATE_DONE,
  type FromIo,
  type MountSpec,
  type ToIo,
} from './protocol';

declare const self: DedicatedWorkerGlobalScope;

type Fd =
  | { kind: 'stdio'; fd: number }
  | { kind: 'file'; path: string; handle: Handle; position: number; append: boolean; writable: boolean }
  | { kind: 'dir'; path: string; entries?: [string, number][] };

const PREOPEN = 3;

let memory: WebAssembly.Memory;
let control: Int32Array;
let controlArgs: BigInt64Array;
let mounts: [string, Backend][] = [];
const fds = new Map<number, Fd>([
  [0, { kind: 'stdio', fd: 0 }],
  [1, { kind: 'stdio', fd: 1 }],
  [2, { kind: 'stdio', fd: 2 }],
  [PREOPEN, { kind: 'dir', path: '/' }],
]);
let nextFd = PREOPEN + 1;

const post = (message: FromIo, transfer: Transferable[] = []) => self.postMessage(message, transfer);

self.onmessage = async (event: MessageEvent<ToIo>) => {
  const message = event.data;
  if (message.type === 'init') {
    memory = message.memory;
    control = new Int32Array(message.control);
    controlArgs = new BigInt64Array(message.control);
    mounts = await Promise.all(message.mounts.map(async (spec) => [normalize(spec.path), await backendFor(spec)] as [string, Backend]));
    if (!mounts.some(([path]) => path === '/')) mounts.push(['/', new MemoryBackend()]);
    if (!mounts.some(([path]) => path === '/dev')) mounts.push(['/dev', new DeviceBackend()]);
    mounts.sort((a, b) => b[0].length - a[0].length);
    post({ type: 'ready' });
  } else if (message.type === 'sync') {
    post({ type: 'synced' });
  } else {
    const { tid, port } = message;
    port.onmessage = () => void serve(tid);
  }
};

async function backendFor(spec: MountSpec): Promise<Backend> {
  switch (spec.kind) {
    case 'http':
      return new HttpBackend(spec.url);
    case 'memory': {
      const backend = new MemoryBackend();
      for (const name of spec.files ?? []) {
        (await backend.open(`/${name}`, { write: true, create: true, exclusive: false, truncate: true })).close();
      }
      return backend;
    }
    case 'overlay':
      return new OverlayBackend(new HttpBackend(spec.url));
    case 'opfs':
      return new OpfsBackend(spec.root, { readonly: spec.readonly, lazy: spec.lazy });
  }
}

async function serve(tid: number) {
  const slot = slotInt(tid);
  const op = control[slot + 1];
  const base = slotArg(tid);
  const args = Array.from({ length: 10 }, (_, i) => controlArgs[base + i]);
  let result: number;
  try {
    result = await calls[IO_CALLS[op]](args);
  } catch (error) {
    if (error instanceof FsError) {
      result = error.errno;
    } else {
      post({ type: 'error', message: `${IO_CALLS[op]}: ${error instanceof Error ? error.stack : String(error)}` });
      result = ERRNO.IO;
    }
  }
  control[slot + 2] = result;
  Atomics.store(control, slot, STATE_DONE);
  Atomics.notify(control, slot);
}

// Memory access; the memory may have grown since the last call.
const bytes = () => new Uint8Array(memory.buffer);
const view = () => new DataView(memory.buffer);
const u32 = (value: bigint) => Number(BigInt.asUintN(32, value));
const decoder = new TextDecoder();
const readString = (pointer: bigint, length: bigint) => decoder.decode(bytes().slice(u32(pointer), u32(pointer) + u32(length)));

function normalize(path: string): string {
  const parts: string[] = [];
  for (const part of path.split('/')) {
    if (part === '' || part === '.') continue;
    if (part === '..') parts.pop();
    else parts.push(part);
  }
  return `/${parts.join('/')}`;
}

/** The backend for an absolute path and the path inside it. */
function resolve(path: string): [Backend, string] {
  for (const [prefix, backend] of mounts) {
    if (prefix === '/') return [backend, path];
    if (path === prefix) return [backend, '/'];
    if (path.startsWith(`${prefix}/`)) return [backend, path.slice(prefix.length)];
  }
  throw new FsError(ERRNO.NOENT);
}

/** Names of mount points directly inside a directory. */
function mountsIn(path: string): string[] {
  const prefix = path === '/' ? '/' : `${path}/`;
  const names = new Set<string>();
  for (const [mount] of mounts) {
    if (mount !== path && mount.startsWith(prefix)) names.add(mount.slice(prefix.length).split('/')[0]);
  }
  return [...names];
}

async function stat(path: string): Promise<Stat | null> {
  const [backend, inner] = resolve(path);
  const found = await backend.stat(inner);
  if (found) return found;
  return mountsIn(path).length ? { kind: 'dir', size: 0, mtime: 0 } : null;
}

function fdOf(value: bigint): Fd {
  const fd = fds.get(Number(value));
  if (!fd) throw new FsError(ERRNO.BADF);
  return fd;
}

function dirPath(value: bigint): string {
  const fd = fdOf(value);
  if (fd.kind !== 'dir') throw new FsError(ERRNO.NOTDIR);
  return fd.path;
}

const pathArg = (dirfd: bigint, pointer: bigint, length: bigint) => normalize(`${dirPath(dirfd)}/${readString(pointer, length)}`);

function writeFilestat(pointer: bigint, filetype: number, size: number, mtimeMs: number) {
  const v = view();
  const at = u32(pointer);
  const time = BigInt(Math.round(mtimeMs)) * 1_000_000n;
  v.setBigUint64(at, 0n, true);
  v.setBigUint64(at + 8, 0n, true);
  v.setUint8(at + 16, filetype);
  v.setBigUint64(at + 24, 1n, true);
  v.setBigUint64(at + 32, BigInt(size), true);
  v.setBigUint64(at + 40, time, true);
  v.setBigUint64(at + 48, time, true);
  v.setBigUint64(at + 56, time, true);
}

/** The buffers of an iovec array. */
function iovecs(pointer: bigint, count: bigint): Uint8Array[] {
  const v = view();
  const buffer = memory.buffer;
  return Array.from({ length: u32(count) }, (_, i) => {
    const at = u32(pointer) + i * 8;
    return new Uint8Array(buffer, v.getUint32(at, true), v.getUint32(at + 4, true));
  });
}

const calls: Record<(typeof IO_CALLS)[number], (a: bigint[]) => number | Promise<number>> = {
  fd_close([fd]) {
    const entry = fdOf(fd);
    if (entry.kind === 'file') entry.handle.close();
    if (Number(fd) > PREOPEN) fds.delete(Number(fd));
    return ERRNO.SUCCESS;
  },

  fd_fdstat_get([fd, out]) {
    const entry = fdOf(fd);
    const v = view();
    const at = u32(out);
    const type = entry.kind === 'stdio' ? FILETYPE.CHARACTER_DEVICE : entry.kind === 'dir' ? FILETYPE.DIRECTORY : FILETYPE.REGULAR_FILE;
    v.setUint8(at, type);
    v.setUint16(at + 2, entry.kind === 'file' && entry.append ? 1 : 0, true);
    // A character device without seek rights reads as a terminal.
    const rights = entry.kind === 'stdio' ? ~0n & ~(1n << 2n) & ~(1n << 5n) : ~0n;
    v.setBigUint64(at + 8, BigInt.asUintN(64, rights), true);
    v.setBigUint64(at + 16, BigInt.asUintN(64, ~0n), true);
    return ERRNO.SUCCESS;
  },

  fd_fdstat_set_flags([fd, flags]) {
    const entry = fdOf(fd);
    if (entry.kind === 'file') entry.append = (Number(flags) & 1) !== 0;
    return ERRNO.SUCCESS;
  },

  async fd_filestat_get([fd, out]) {
    const entry = fdOf(fd);
    if (entry.kind === 'stdio') writeFilestat(out, FILETYPE.CHARACTER_DEVICE, 0, 0);
    else if (entry.kind === 'file') writeFilestat(out, FILETYPE.REGULAR_FILE, entry.handle.size(), Date.now());
    else {
      const found = await stat(entry.path);
      writeFilestat(out, FILETYPE.DIRECTORY, 0, found?.mtime ?? 0);
    }
    return ERRNO.SUCCESS;
  },

  fd_filestat_set_size([fd, size]) {
    const entry = fdOf(fd);
    if (entry.kind !== 'file') return ERRNO.BADF;
    entry.handle.truncate(Number(size));
    return ERRNO.SUCCESS;
  },

  fd_filestat_set_times() {
    return ERRNO.SUCCESS;
  },

  fd_prestat_get([fd, out]) {
    if (Number(fd) !== PREOPEN) return ERRNO.BADF;
    view().setUint8(u32(out), 0);
    view().setUint32(u32(out) + 4, 1, true);
    return ERRNO.SUCCESS;
  },

  fd_prestat_dir_name([fd, pointer, length]) {
    if (Number(fd) !== PREOPEN) return ERRNO.BADF;
    if (u32(length) >= 1) bytes()[u32(pointer)] = '/'.charCodeAt(0);
    return ERRNO.SUCCESS;
  },

  fd_read([fd, iovs, count, out]) {
    const entry = fdOf(fd);
    let total = 0;
    if (entry.kind === 'file') {
      for (const buffer of iovecs(iovs, count)) {
        const n = entry.handle.read(entry.position, buffer);
        entry.position += n;
        total += n;
        if (n < buffer.length) break;
      }
    } else if (entry.kind === 'dir') {
      return ERRNO.ISDIR;
    }
    // stdin: end of input.
    view().setUint32(u32(out), total, true);
    return ERRNO.SUCCESS;
  },

  async fd_readdir([fd, buf, length, cookie, out]) {
    const entry = fdOf(fd);
    if (entry.kind !== 'dir') return ERRNO.NOTDIR;
    if (!entry.entries || cookie === 0n) {
      const [backend, inner] = resolve(entry.path);
      const names = new Set(await backend.list(inner).catch(() => [] as string[]));
      for (const name of mountsIn(entry.path)) names.add(name);
      const list: [string, number][] = [
        ['.', FILETYPE.DIRECTORY],
        ['..', FILETYPE.DIRECTORY],
      ];
      for (const name of names) {
        const child = entry.path === '/' ? `/${name}` : `${entry.path}/${name}`;
        const found = await stat(child);
        list.push([name, found?.kind === 'dir' ? FILETYPE.DIRECTORY : FILETYPE.REGULAR_FILE]);
      }
      entry.entries = list;
    }
    const encoder = new TextEncoder();
    const capacity = u32(length);
    const record = new Uint8Array(capacity + 1024);
    const recordView = new DataView(record.buffer);
    let used = 0;
    for (let i = Number(cookie); i < entry.entries.length && used < capacity; i++) {
      const [name, type] = entry.entries[i];
      const encoded = encoder.encode(name);
      recordView.setBigUint64(used, BigInt(i + 1), true);
      recordView.setBigUint64(used + 8, BigInt(i + 1), true);
      recordView.setUint32(used + 16, encoded.length, true);
      recordView.setUint8(used + 20, type);
      record.set(encoded, used + 24);
      used += 24 + encoded.length;
    }
    used = Math.min(used, capacity);
    bytes().set(record.subarray(0, used), u32(buf));
    view().setUint32(u32(out), used, true);
    return ERRNO.SUCCESS;
  },

  fd_seek([fd, offset, whence, out]) {
    const entry = fdOf(fd);
    if (entry.kind !== 'file') return ERRNO.SPIPE;
    const base = [0, entry.position, entry.handle.size()][Number(whence)];
    if (base === undefined) return ERRNO.INVAL;
    const position = base + Number(offset);
    if (position < 0) return ERRNO.INVAL;
    entry.position = position;
    view().setBigUint64(u32(out), BigInt(position), true);
    return ERRNO.SUCCESS;
  },

  fd_sync([fd]) {
    const entry = fdOf(fd);
    if (entry.kind === 'file') entry.handle.flush();
    return ERRNO.SUCCESS;
  },

  fd_write([fd, iovs, count, out]) {
    const entry = fdOf(fd);
    let total = 0;
    const buffers = iovecs(iovs, count);
    if (entry.kind === 'stdio') {
      const size = buffers.reduce((sum, b) => sum + b.length, 0);
      const data = new Uint8Array(size);
      for (const buffer of buffers) {
        data.set(buffer, total);
        total += buffer.length;
      }
      if (entry.fd === 1 || entry.fd === 2) {
        post({ type: 'output', fd: entry.fd, data }, [data.buffer]);
      }
    } else if (entry.kind === 'file') {
      if (!entry.writable) return ERRNO.BADF;
      if (entry.append) entry.position = entry.handle.size();
      for (const buffer of buffers) {
        // Copy out of shared memory: backends may hand the bytes on.
        const n = entry.handle.write(entry.position, buffer.slice());
        entry.position += n;
        total += n;
      }
    } else {
      return ERRNO.ISDIR;
    }
    view().setUint32(u32(out), total, true);
    return ERRNO.SUCCESS;
  },

  async path_create_directory([dirfd, pointer, length]) {
    const path = pathArg(dirfd, pointer, length);
    if (await stat(path)) return ERRNO.EXIST;
    const [backend, inner] = resolve(path);
    await backend.mkdir(inner);
    return ERRNO.SUCCESS;
  },

  async path_filestat_get([dirfd, , pointer, length, out]) {
    const path = pathArg(dirfd, pointer, length);
    const found = await stat(path);
    if (!found) return ERRNO.NOENT;
    writeFilestat(out, found.kind === 'dir' ? FILETYPE.DIRECTORY : FILETYPE.REGULAR_FILE, found.size, found.mtime);
    return ERRNO.SUCCESS;
  },

  async path_open([dirfd, , pointer, length, oflags, rights, , fdflags, out]) {
    const path = pathArg(dirfd, pointer, length);
    const o = Number(oflags);
    const create = (o & 1) !== 0;
    const directory = (o & 2) !== 0;
    const exclusive = (o & 4) !== 0;
    const truncate = (o & 8) !== 0;
    const writable = (rights & (1n << 6n)) !== 0n;
    const found = await stat(path);
    let entry: Fd;
    if (found?.kind === 'dir') {
      if (exclusive && create) return ERRNO.EXIST;
      if (writable && !directory) return ERRNO.ISDIR;
      entry = { kind: 'dir', path };
    } else {
      if (directory) return found ? ERRNO.NOTDIR : ERRNO.NOENT;
      const [backend, inner] = resolve(path);
      if (!found && create && !(await stat(parent(path)))) return ERRNO.NOENT;
      if (backend.readonly && (writable || create || truncate) && !(found && !writable && !truncate)) return ERRNO.ROFS;
      const handle = await backend.open(inner, { write: writable, create, exclusive, truncate });
      entry = { kind: 'file', path, handle, position: 0, append: (Number(fdflags) & 1) !== 0, writable };
    }
    const fd = nextFd++;
    fds.set(fd, entry);
    view().setUint32(u32(out), fd, true);
    return ERRNO.SUCCESS;
  },

  async path_remove_directory([dirfd, pointer, length]) {
    const path = pathArg(dirfd, pointer, length);
    const [backend, inner] = resolve(path);
    await backend.rmdir(inner);
    return ERRNO.SUCCESS;
  },

  async path_rename([dirfd, pointer, length, newDirfd, newPointer, newLength]) {
    const from = pathArg(dirfd, pointer, length);
    const to = pathArg(newDirfd, newPointer, newLength);
    const [backend, inner] = resolve(from);
    const [target, targetInner] = resolve(to);
    if (backend !== target) return ERRNO.NOTSUP;
    await backend.rename(inner, targetInner);
    return ERRNO.SUCCESS;
  },

  async path_unlink_file([dirfd, pointer, length]) {
    const path = pathArg(dirfd, pointer, length);
    const [backend, inner] = resolve(path);
    await backend.unlink(inner);
    return ERRNO.SUCCESS;
  },
};
