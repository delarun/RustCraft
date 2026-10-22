// Shared definitions for the JVM process: the VM (rjava.wasm, a WASI
// "threads" program) runs in Web Workers that share one WebAssembly.Memory.
// Each VM thread is a worker; file system calls are forwarded to a single I/O
// worker that owns the file descriptor table, through a per-thread request
// slot in a shared control block (the caller blocks with Atomics.wait).

/** Most VM threads alive at once (request slots in the control block). */
export const MAX_THREADS = 1024;

/** Control block header (Int32 indices). */
export const HEADER_INTS = 16;
export const NEXT_TID = 0;

/** Per-thread request slot: 16 64-bit words after the header. Int32 view:
 *  [0] state, [1] operation, [2] result (errno); BigInt64 view: words 2.. are
 *  the call's arguments. */
export const SLOT_WORDS = 16;
export const SLOT_ARGS = 2;
export const STATE_IDLE = 0;
export const STATE_PENDING = 1;
export const STATE_DONE = 2;

export const CONTROL_BYTES = HEADER_INTS * 4 + MAX_THREADS * SLOT_WORDS * 8;

/** Int32 index of a thread's slot. */
export const slotInt = (tid: number) => HEADER_INTS + (tid % MAX_THREADS) * SLOT_WORDS * 2;
/** BigInt64 index of a thread's first argument. */
export const slotArg = (tid: number) => HEADER_INTS / 2 + (tid % MAX_THREADS) * SLOT_WORDS + SLOT_ARGS;

/** WASI functions served by the I/O worker, in operation-number order. */
export const IO_CALLS = [
  'fd_close',
  'fd_fdstat_get',
  'fd_fdstat_set_flags',
  'fd_filestat_get',
  'fd_filestat_set_size',
  'fd_filestat_set_times',
  'fd_prestat_get',
  'fd_prestat_dir_name',
  'fd_read',
  'fd_readdir',
  'fd_seek',
  'fd_sync',
  'fd_write',
  'path_create_directory',
  'path_filestat_get',
  'path_open',
  'path_remove_directory',
  'path_rename',
  'path_unlink_file',
] as const;
export type IoCall = (typeof IO_CALLS)[number];

/** A file system mounted into the VM's view. */
export type MountSpec =
  /** Read-only files served over HTTP (see the dev-fs plugin in vite.config.ts). */
  | { path: string; kind: 'http'; url: string }
  /** A writable in-memory directory, optionally holding empty files (names in its root). */
  | { path: string; kind: 'memory'; files?: string[] }
  /** HTTP files with writes kept in memory. */
  | { path: string; kind: 'overlay'; url: string }
  /** A directory of the origin private file system (see opfs.ts). */
  | { path: string; kind: 'opfs'; root: string; readonly?: boolean; lazy?: LazyObjects };

/** Content-addressed files fetched on first use (see opfs.ts): `<dir>/<xx>/<sha1>`
 *  for the hashes listed in an asset index (`objects` of `index`), from `<url>/<xx>/<sha1>`. */
export interface LazyObjects {
  dir: string;
  index: string;
  url: string;
}

export interface ProcessConfig {
  args: string[];
  env: Record<string, string>;
  mounts: MountSpec[];
  /** Initial memory size in 64 KiB pages (default: the module's minimum). */
  initialPages?: number;
  /** ws://host:port of rustcraft-bridge, for TCP connections (see net.ts). */
  netBridge?: string;
}

// Messages.
export type ToIo =
  | { type: 'init'; memory: WebAssembly.Memory; control: SharedArrayBuffer; mounts: MountSpec[] }
  | { type: 'port'; tid: number; port: MessagePort }
  /** Answered with 'synced' after all output sent before it. */
  | { type: 'sync' };
export type FromIo =
  | { type: 'output'; fd: number; data: Uint8Array }
  | { type: 'ready' }
  | { type: 'synced' }
  | { type: 'error'; message: string };

export interface ThreadStart {
  type: 'start';
  module: WebAssembly.Module;
  memory: WebAssembly.Memory;
  control: SharedArrayBuffer;
  tid: number;
  /** Start argument for wasi_thread_start; the main thread (tid 0) runs _start. */
  arg: number;
  args: string[];
  env: string[];
  io: MessagePort;
  input: SharedArrayBuffer;
}
export type FromThread =
  | { type: 'spawn'; tid: number; arg: number }
  | { type: 'exit'; code: number }
  | { type: 'done'; tid: number }
  | { type: 'error'; tid: number; message: string }
  | NetMessage
  | DisplayMessage;

/** From a VM thread to the page's NetBridge (see net.ts). */
export type NetMessage =
  | { type: 'net-open'; block: number; address: string }
  | { type: 'net-kick'; block: number };

/** From the thread that owns a window to the page (see display.ts). */
export type DisplayMessage =
  | { type: 'window'; id: number; width: number; height: number; visible: boolean }
  | { type: 'window-destroyed'; id: number }
  | { type: 'title'; id: number; title: string }
  | { type: 'frame'; id: number; bitmap: ImageBitmap }
  | { type: 'cursor'; kind: number }
  | { type: 'relative-mouse'; enabled: boolean }
  | { type: 'clipboard'; text: string }
  | { type: 'open-url'; url: string }
  /** OpenAL output: the address of the ring in the VM's memory. */
  | { type: 'audio'; ring: number };

/** The input block (Int32 indices) the page writes and the VM reads. */
export const INPUT = {
  /** Events written (count) and read. */
  WRITTEN: 0,
  READ: 1,
  /** Page state: CSS size, pixel size, device pixel ratio × 1000, focus,
   *  pointer lock, screen size, frames shown. */
  CSS_WIDTH: 2,
  CSS_HEIGHT: 3,
  PIXEL_WIDTH: 4,
  PIXEL_HEIGHT: 5,
  DPR: 6,
  FOCUSED: 7,
  LOCKED: 8,
  SCREEN_WIDTH: 9,
  SCREEN_HEIGHT: 10,
  FRAMES_SHOWN: 11,
  /** A ring of events, EVENT_INTS words each (see vm/crates/jvm/src/web/sdl.rs). */
  RING: 16,
  EVENTS: 1024,
  EVENT_INTS: 8,
} as const;
export const INPUT_BYTES = (INPUT.RING + INPUT.EVENTS * INPUT.EVENT_INTS) * 4;

// WASI constants.
export const ERRNO = {
  SUCCESS: 0,
  ACCES: 2,
  BADF: 8,
  EXIST: 20,
  INVAL: 28,
  IO: 29,
  ISDIR: 31,
  NOENT: 44,
  NOSYS: 52,
  NOTDIR: 54,
  NOTEMPTY: 55,
  NOTSUP: 58,
  PERM: 63,
  ROFS: 69,
  SPIPE: 70,
} as const;

export const FILETYPE = { UNKNOWN: 0, CHARACTER_DEVICE: 2, DIRECTORY: 3, REGULAR_FILE: 4 } as const;
