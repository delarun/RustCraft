// One VM thread: instantiates rjava.wasm on the shared memory and runs
// _start (the main thread) or wasi_thread_start (threads it spawns). Calls
// that need no shared state (clocks, randomness, sleeping, arguments) are
// answered here; file system calls go to the I/O worker.

import { platformImports } from './platform';
import {
  ERRNO,
  IO_CALLS,
  NEXT_TID,
  MAX_THREADS,
  slotArg,
  slotInt,
  STATE_IDLE,
  STATE_PENDING,
  type FromThread,
  type ThreadStart,
} from './protocol';

declare const self: DedicatedWorkerGlobalScope;

/** Thrown by proc_exit to unwind the wasm stack. */
class Exit {
  constructor(readonly code: number) {}
}

self.onmessage = (event: MessageEvent<ThreadStart>) => void run(event.data);

function post(message: FromThread) {
  self.postMessage(message);
}

type HostFn = (...args: number[]) => number;
interface VmExports {
  _start(): void;
  wasi_thread_start(tid: number, arg: number): void;
  rjava_jit_step: HostFn;
  rjava_jit_invoke: HostFn;
  rjava_jit_select: HostFn;
  rjava_jit_subtype: HostFn;
  rjava_jit_pure: HostFn;
  rjava_jit_fmod: (a: number, b: number) => number;
  rjava_jit_monitor: HostFn;
  rjava_jit_interpret: HostFn;
  rjava_jit_safepoint: HostFn;
  rjava_jit_overflow: HostFn;
  rjava_jit_call: HostFn;
  rjava_jit_table_size(size: number): void;
  __indirect_function_table: WebAssembly.Table;
  malloc(size: number): number;
  free(pointer: number): void;
}

/** This worker's engine stack in bytes, measured by recursing until it
 *  overflows with frames of 100 i64 locals (a slot each, plus ~56 bytes, in
 *  V8's baseline tier). */
const nativeStack = (() => {
  const leb = (n: number) => {
    const out: number[] = [];
    do {
      let byte = n & 0x7f;
      n >>>= 7;
      if (n) byte |= 0x80;
      out.push(byte);
    } while (n);
    return out;
  };
  const LOCALS = 100;
  // (func $f (param i32) (result i32) (local 100 i64) counter++ (call $f (local.get 0)))
  const body = [1, ...leb(LOCALS), 0x7e, 0x23, 0, 0x41, 1, 0x6a, 0x24, 0, 0x20, 0, 0x10, 0, 0x0b];
  const section = (id: number, bytes: number[]) => [id, ...leb(bytes.length), ...bytes];
  const probe = new WebAssembly.Instance(
    new WebAssembly.Module(
      new Uint8Array([
        0, 97, 115, 109, 1, 0, 0, 0,
        ...section(1, [1, 0x60, 1, 0x7f, 1, 0x7f]),
        ...section(3, [1, 0]),
        ...section(6, [1, 0x7f, 1, 0x41, 0, 0x0b]),
        ...section(7, [2, 1, 102, 0, 0, 1, 99, 3, 0]),
        ...section(10, [1, ...leb(body.length), ...body]),
      ]),
    ),
  ).exports as { f: (x: number) => number; c: WebAssembly.Global };
  try {
    probe.f(0);
  } catch {
    // RangeError: the stack is full.
  }
  return (probe.c.value as number) * (LOCALS * 8 + 56);
})();

async function run(start: ThreadStart) {
  const { memory, tid } = start;
  const control = new Int32Array(start.control);
  const controlArgs = new BigInt64Array(start.control);
  const slot = slotInt(tid);
  const argBase = slotArg(tid);
  const bytes = () => new Uint8Array(memory.buffer);
  const view = () => new DataView(memory.buffer);

  const forward = (op: number) =>
    (...args: (number | bigint)[]) => {
      for (let i = 0; i < args.length; i++) controlArgs[argBase + i] = BigInt(args[i]);
      control[slot + 1] = op;
      Atomics.store(control, slot, STATE_PENDING);
      start.io.postMessage(0);
      while (Atomics.load(control, slot) === STATE_PENDING) Atomics.wait(control, slot, STATE_PENDING);
      const result = control[slot + 2];
      Atomics.store(control, slot, STATE_IDLE);
      return result;
    };

  const encoder = new TextEncoder();
  const strings = (list: string[]) => list.map((s) => encoder.encode(`${s}\0`));
  const argv = strings(start.args);
  const envv = strings(start.env);
  const sizes = (list: Uint8Array[], countPtr: number, sizePtr: number) => {
    const v = view();
    v.setUint32(countPtr >>> 0, list.length, true);
    v.setUint32(sizePtr >>> 0, list.reduce((sum, s) => sum + s.length, 0), true);
    return ERRNO.SUCCESS;
  };
  const fill = (list: Uint8Array[], pointers: number, buffer: number) => {
    const v = view();
    const b = bytes();
    let at = buffer >>> 0;
    list.forEach((s, i) => {
      v.setUint32((pointers >>> 0) + i * 4, at, true);
      b.set(s, at);
      at += s.length;
    });
    return ERRNO.SUCCESS;
  };

  // Wall-clock nanoseconds; timeOrigin + now() agrees across workers.
  const nowNs = () => BigInt(Math.round((performance.timeOrigin + performance.now()) * 1e6));
  const sleeper = new Int32Array(new SharedArrayBuffer(4));

  const wasi: Record<string, (...args: never[]) => number | void> = {
    args_sizes_get: (count: number, size: number) => sizes(argv, count, size),
    args_get: (pointers: number, buffer: number) => fill(argv, pointers, buffer),
    environ_sizes_get: (count: number, size: number) => sizes(envv, count, size),
    environ_get: (pointers: number, buffer: number) => fill(envv, pointers, buffer),
    clock_time_get: (_id: number, _precision: bigint, out: number) => {
      view().setBigUint64(out >>> 0, nowNs(), true);
      return ERRNO.SUCCESS;
    },
    random_get: (buffer: number, length: number) => {
      const chunk = new Uint8Array(65536);
      for (let done = 0; done < length; done += chunk.length) {
        const n = Math.min(chunk.length, length - done);
        crypto.getRandomValues(chunk.subarray(0, n));
        bytes().set(chunk.subarray(0, n), (buffer >>> 0) + done);
      }
      return ERRNO.SUCCESS;
    },
    sched_yield: () => ERRNO.SUCCESS,
    poll_oneoff: (inPtr: number, outPtr: number, count: number, eventsOut: number) => {
      // Clock subscriptions sleep until the earliest deadline; fd
      // subscriptions are reported ready at once.
      const v = view();
      const now = nowNs();
      let deadline: bigint | null = null;
      let ready = 0;
      const events: [bigint, number, number][] = [];
      for (let i = 0; i < count; i++) {
        const sub = (inPtr >>> 0) + i * 48;
        const userdata = v.getBigUint64(sub, true);
        const tag = v.getUint8(sub + 8);
        if (tag === 0) {
          const timeout = v.getBigUint64(sub + 24, true);
          const absolute = (v.getUint16(sub + 40, true) & 1) !== 0;
          const at = absolute ? timeout : now + timeout;
          if (deadline === null || at < deadline) deadline = at;
          events.push([userdata, 0, -1]);
        } else {
          events.push([userdata, tag, 1]);
          ready++;
        }
      }
      if (ready === 0 && deadline !== null && deadline > now) {
        Atomics.wait(sleeper, 0, 0, Number(deadline - now) / 1e6);
      }
      let written = 0;
      for (const [userdata, type, isReady] of events) {
        if (ready > 0 && isReady < 0) continue;
        const event = (outPtr >>> 0) + written * 32;
        v.setBigUint64(event, userdata, true);
        v.setUint16(event + 8, ERRNO.SUCCESS, true);
        v.setUint8(event + 10, type);
        v.setBigUint64(event + 16, 0n, true);
        v.setUint16(event + 24, 0, true);
        written++;
      }
      v.setUint32(eventsOut >>> 0, written, true);
      return ERRNO.SUCCESS;
    },
    proc_exit: (code: number) => {
      post({ type: 'exit', code });
      throw new Exit(code);
    },
    sock_accept: () => ERRNO.NOSYS,
    sock_shutdown: () => ERRNO.NOSYS,
  };
  IO_CALLS.forEach((name, op) => (wasi[name] = forward(op)));

  const spawn = (arg: number) => {
    const child = Atomics.add(control, NEXT_TID, 1);
    if (child >= MAX_THREADS * 64) return -1;
    post({ type: 'spawn', tid: child, arg });
    return child;
  };

  // The JIT (crates/jit-web): compiled methods are modules on the same
  // memory, instantiated in this worker on first use, calling each other
  // through this worker's table (unfilled entries: rjava_jit_call).
  let vm: VmExports;
  let table: WebAssembly.Table;
  const ensureTable = (index: number) => {
    if (index >= table.length) {
      table.grow(Math.max(index + 1, table.length * 2, 1024) - table.length, vm.rjava_jit_call);
      vm.rjava_jit_table_size(table.length);
    }
    return table.length;
  };
  const jit = {
    instantiate(module: number, pointer: number, length: number, method: number, tableSize: number) {
      {
        const code = new WebAssembly.Module(bytes().slice(pointer >>> 0, (pointer >>> 0) + length));
        ensureTable(Math.max(tableSize - 1, method));
        const compiled = new WebAssembly.Instance(code, {
          vm: {
            memory,
            table,
            step: vm.rjava_jit_step,
            invoke: vm.rjava_jit_invoke,
            select: vm.rjava_jit_select,
            interpret: vm.rjava_jit_interpret,
            safepoint: vm.rjava_jit_safepoint,
            overflow: vm.rjava_jit_overflow,
            call: vm.rjava_jit_call,
            subtype: vm.rjava_jit_subtype,
            pure: vm.rjava_jit_pure,
            fmod: vm.rjava_jit_fmod,
            monitor: vm.rjava_jit_monitor,
          },
        }).exports as { run: WebAssembly.ExportValue; call: WebAssembly.ExportValue };
        table.set(method, compiled.call);
        // Into the VM instance's own table: Rust calls it from there.
        const functions = vm.__indirect_function_table;
        const index = functions.grow(1);
        functions.set(index, compiled.run);
        void module;
        return index;
      }
    },
    table_ensure: ensureTable,
    native_stack: () => nativeStack,
    validate: (pointer: number, length: number) => WebAssembly.validate(bytes().slice(pointer >>> 0, (pointer >>> 0) + length)),
  };

  const noJspi = start.env.some((e) => e.startsWith('RJAVA_NO_JSPI='));
  const platform = platformImports({
    memory,
    input: start.input,
    post: (message, transfer = []) => self.postMessage(message, transfer),
    // Addresses above 2 GiB arrive as negative i32s.
    malloc: (size) => vm.malloc(size) >>> 0,
    free: (pointer) => vm.free(pointer),
    debugGl: start.env.some((e) => e.startsWith('RJAVA_GL_DEBUG=')),
    stats: start.env.some((e) => e.startsWith('RJAVA_WEB_STATS=')),
    noJspi,
  });

  try {
    const instance = new WebAssembly.Instance(start.module, {
      env: { memory },
      wasi_snapshot_preview1: wasi,
      wasi: { 'thread-spawn': spawn },
      rjava_jit: jit,
      ...platform,
      // (Suspending imports are valid import values; TypeScript's lib predates JSPI.)
    } as unknown as WebAssembly.Imports);
    vm = instance.exports as unknown as VmExports;
    table = new WebAssembly.Table({ element: 'anyfunc', initial: 0 });
    // Entered with JSPI: imports may suspend the thread (see platform.ts).
    const enter = <A extends unknown[]>(f: (...args: A) => void) => (noJspi ? f : WebAssembly.promising(f));
    if (tid === 0) {
      await enter(vm._start)();
      post({ type: 'exit', code: 0 });
    } else {
      await enter(vm.wasi_thread_start)(tid, start.arg);
      post({ type: 'done', tid });
    }
  } catch (error) {
    if (!(error instanceof Exit)) {
      const size = `memory ${(memory.buffer.byteLength / 2 ** 20).toFixed(0)} MiB`;
      const message = error instanceof Error ? `${error.message} (${size})\n${error.stack ?? ''}` : String(error);
      post({ type: 'error', tid, message });
    }
  }
  self.close();
}
