// Main-thread side of a JVM process: compiles rjava.wasm, creates the shared
// memory, the I/O worker and a worker per VM thread (threads spawned by the VM
// are started from here, so a blocked thread never waits on its own event
// loop), and collects output and the exit status.

import {
  CONTROL_BYTES,
  INPUT_BYTES,
  NEXT_TID,
  type DisplayMessage,
  type FromIo,
  type FromThread,
  type ProcessConfig,
  type ThreadStart,
  type ToIo,
} from './protocol';
import { NetBridge } from './net';

export interface ProcessCallbacks {
  /** Complete lines written to stdout (1) or stderr (2). */
  onOutput(fd: number, line: string): void;
  onExit(code: number): void;
  /** A worker failed (a trap ends the process; I/O errors are reported). */
  onError(message: string): void;
  /** Windows and frames of the VM's threads (see display.ts). */
  onDisplay?(message: DisplayMessage): void;
}

export class JvmProcess {
  private threads = new Map<number, Worker>();
  private io: Worker;
  private control = new SharedArrayBuffer(CONTROL_BYTES);
  readonly memory: WebAssembly.Memory;
  private pending = ['', ''];
  private decoders = [new TextDecoder(), new TextDecoder()];
  private exited = false;
  private net: NetBridge;
  private exitCode = 0;

  private constructor(
    private module: WebAssembly.Module,
    limits: { initial: number; maximum: number },
    private config: ProcessConfig,
    private callbacks: ProcessCallbacks,
    /** Input from the page (see protocol INPUT, display.ts). */
    readonly input: SharedArrayBuffer,
  ) {
    this.memory = new WebAssembly.Memory({ ...limits, shared: true });
    this.net = new NetBridge(this.memory, config.netBridge);
    this.io = new Worker(new URL('./io.worker.ts', import.meta.url), { type: 'module', name: 'jvm io' });
    this.io.onmessage = (event: MessageEvent<FromIo>) => this.fromIo(event.data);
    this.io.onerror = (event) => this.fail(`I/O worker: ${event.message}`);
  }

  static async start(
    wasm: ArrayBuffer,
    config: ProcessConfig,
    callbacks: ProcessCallbacks,
    input = new SharedArrayBuffer(INPUT_BYTES),
  ): Promise<JvmProcess> {
    const limits = memoryImport(new Uint8Array(wasm));
    if (config.initialPages) limits.initial = Math.min(limits.maximum, Math.max(limits.initial, config.initialPages));
    const module = await WebAssembly.compile(wasm);
    const process = new JvmProcess(module, limits, config, callbacks, input);
    await process.init();
    Atomics.store(new Int32Array(process.control), NEXT_TID, 1);
    process.spawn(0, 0);
    return process;
  }

  private init(): Promise<void> {
    return new Promise((resolve) => {
      const ready = this.io.onmessage;
      this.io.onmessage = (event: MessageEvent<FromIo>) => {
        if (event.data.type === 'ready') {
          this.io.onmessage = ready;
          resolve();
        } else {
          ready?.call(this.io, event);
        }
      };
      this.toIo({ type: 'init', memory: this.memory, control: this.control, mounts: this.config.mounts });
    });
  }

  kill() {
    if (this.exited) return;
    this.exited = true;
    this.stopThreads();
    this.io.terminate();
  }

  private stopThreads() {
    for (const worker of this.threads.values()) worker.terminate();
    this.threads.clear();
  }

  private toIo(message: ToIo, transfer: Transferable[] = []) {
    this.io.postMessage(message, transfer);
  }

  private spawn(tid: number, arg: number) {
    if (this.exited) return;
    const worker = new Worker(new URL('./thread.worker.ts', import.meta.url), { type: 'module', name: `jvm thread ${tid}` });
    const channel = new MessageChannel();
    this.toIo({ type: 'port', tid, port: channel.port1 }, [channel.port1]);
    worker.onmessage = (event: MessageEvent<FromThread>) => this.fromThread(event.data);
    worker.onerror = (event) => this.fail(`thread ${tid}: ${event.message}`);
    const start: ThreadStart = {
      type: 'start',
      module: this.module,
      memory: this.memory,
      control: this.control,
      tid,
      arg,
      args: this.config.args,
      // The VM sizes its thread pools by the processors it sees.
      env: Object.entries({ RJAVA_CPUS: String(navigator.hardwareConcurrency), ...this.config.env }).map(
        ([key, value]) => `${key}=${value}`,
      ),
      io: channel.port2,
      input: this.input,
    };
    worker.postMessage(start, [channel.port2]);
    this.threads.set(tid, worker);
  }

  private fromThread(message: FromThread) {
    switch (message.type) {
      case 'spawn':
        this.spawn(message.tid, message.arg);
        break;
      case 'done':
        this.threads.delete(message.tid);
        break;
      case 'exit':
        this.exit(message.code);
        break;
      case 'error':
        this.fail(`thread ${message.tid}: ${message.message}`);
        break;
      case 'net-open':
        this.net.open(message.block, message.address);
        break;
      case 'net-kick':
        this.net.kick(message.block);
        break;
      default:
        this.callbacks.onDisplay?.(message);
    }
  }

  private fromIo(message: FromIo) {
    if (message.type === 'output') {
      const index = message.fd - 1;
      const text = this.pending[index] + this.decoders[index].decode(message.data, { stream: true });
      const lines = text.split('\n');
      this.pending[index] = lines.pop()!;
      for (const line of lines) this.callbacks.onOutput(message.fd, line);
    } else if (message.type === 'synced') {
      this.flush();
      this.io.terminate();
      this.callbacks.onExit(this.exitCode);
    } else if (message.type === 'error') {
      this.callbacks.onError(message.message);
    }
  }

  private flush() {
    this.pending.forEach((rest, index) => rest && this.callbacks.onOutput(index + 1, rest));
    this.pending = ['', ''];
  }

  private exit(code: number) {
    if (this.exited) return;
    this.exited = true;
    this.exitCode = code;
    this.stopThreads();
    // Output the I/O worker has yet to send arrives before its reply.
    this.toIo({ type: 'sync' });
  }

  private fail(message: string) {
    if (this.exited) return;
    this.callbacks.onError(message);
    this.exit(134);
  }
}

/** The limits of the module's imported memory (env.memory). */
function memoryImport(wasm: Uint8Array): { initial: number; maximum: number } {
  let at = 8;
  const leb = () => {
    let result = 0;
    let shift = 0;
    for (;;) {
      const byte = wasm[at++];
      result += (byte & 0x7f) * 2 ** shift;
      if (!(byte & 0x80)) return result;
      shift += 7;
    }
  };
  const name = () => {
    const length = leb();
    at += length;
  };
  while (at < wasm.length) {
    const id = wasm[at++];
    const size = leb();
    const end = at + size;
    if (id === 2) {
      for (let count = leb(); count > 0; count--) {
        name();
        name();
        const kind = wasm[at++];
        if (kind === 2) {
          const flags = wasm[at++];
          const initial = leb();
          const maximum = flags & 1 ? leb() : 65536;
          return { initial, maximum };
        }
        if (kind === 0) leb();
        else if (kind === 1) {
          at++;
          const flags = wasm[at++];
          leb();
          if (flags & 1) leb();
        } else if (kind === 3) at += 2;
        else if (kind === 4) {
          at++;
          leb();
        }
      }
    }
    at = end;
  }
  throw new Error('rjava.wasm imports no memory');
}
