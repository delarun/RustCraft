// Child processes of a JVM process (WASI has none): a program asks through its rc_proc import
// (thread.worker.ts) to start one of the programs its config names (ProcessConfig.children),
// which runs as a process of its own on the same mounts; the program writes its stdin, reads
// its stdout and stderr, and waits for its exit. Each call is a JSON request; the result goes
// into the caller's memory (a buffer, then a cell the caller waits on).

import { JvmProcess } from './process';
import { ERRNO, type ChildPrograms, type MountSpec, type ProcMessage } from './protocol';

/** The status of a child that was stopped (as a shell reports SIGKILL). */
const KILLED = 137;

interface Read {
  buffer: number;
  capacity: number;
  cell: number;
}

class Pipe {
  chunks: Uint8Array[] = [];
  closed = false;
  reader: Read | null = null;
}

class Child {
  process: JvmProcess | null = null;
  /** stdout, stderr. */
  pipes = [new Pipe(), new Pipe()];
  code: number | null = null;
  waiters: number[] = [];
  /** Input written before the process was up; null closes it. */
  early: (Uint8Array | null)[] = [];
  dropped = false;
}

interface Request {
  op: 'spawn' | 'read' | 'write' | 'close' | 'wait' | 'kill' | 'drop';
  id?: number;
  fd?: number;
  program?: string;
  args?: string[];
  cwd?: string;
}

export class Children {
  private next = 1;
  private children = new Map<number, Child>();
  private wasm = new Map<string, Promise<ArrayBuffer>>();

  constructor(
    private memory: WebAssembly.Memory,
    private programs: ChildPrograms,
    private mounts: MountSpec[],
    private log: (line: string) => void,
  ) {}

  handle({ request, buffer, capacity, cell }: ProcMessage) {
    const r = JSON.parse(request) as Request;
    if (r.op === 'spawn') {
      this.reply(cell, this.spawn(r));
      return;
    }
    const child = this.children.get(r.id ?? 0);
    if (!child) {
      this.reply(cell, -ERRNO.BADF);
      return;
    }
    switch (r.op) {
      case 'write': {
        const data = new Uint8Array(this.memory.buffer).slice(buffer, buffer + capacity);
        if (child.process) child.process.writeStdin(data);
        else child.early.push(data);
        this.reply(cell, capacity);
        break;
      }
      case 'close':
        if (child.process) child.process.writeStdin(null);
        else child.early.push(null);
        this.reply(cell, 0);
        break;
      case 'read': {
        const pipe = child.pipes[(r.fd ?? 1) === 2 ? 1 : 0];
        pipe.reader = { buffer, capacity, cell };
        this.pump(pipe);
        break;
      }
      case 'wait':
        if (child.code !== null) this.reply(cell, child.code);
        else child.waiters.push(cell);
        break;
      case 'kill':
        child.process?.kill();
        this.finish(child, KILLED);
        this.reply(cell, 0);
        break;
      case 'drop':
        child.dropped = true;
        if (child.code === null) {
          child.process?.kill();
          this.finish(child, KILLED);
        }
        this.children.delete(r.id!);
        this.reply(cell, 0);
        break;
    }
  }

  /** Stops every child (the parent ended). */
  killAll() {
    for (const child of this.children.values()) {
      child.process?.kill();
      this.finish(child, KILLED);
    }
    this.children.clear();
  }

  private spawn(r: Request): number {
    const program = this.programs[r.program ?? ''];
    if (!program) return -ERRNO.NOENT;
    const id = this.next++;
    const child = new Child();
    this.children.set(id, child);
    void this.start(child, program, r).catch((error) => {
      const message = `${r.program}: ${error instanceof Error ? error.message : String(error)}\n`;
      this.log(message.trim());
      child.pipes[1].chunks.push(new TextEncoder().encode(message));
      this.finish(child, 127);
    });
    return id;
  }

  private async start(child: Child, program: ChildPrograms[string], r: Request) {
    let wasm = this.wasm.get(program.wasm);
    if (!wasm) {
      wasm = fetch(program.wasm).then((response) => {
        if (!response.ok) throw new Error(`${program.wasm}: HTTP ${response.status}`);
        return response.arrayBuffer();
      });
      this.wasm.set(program.wasm, wasm);
      wasm.catch(() => this.wasm.delete(program.wasm));
    }
    const bytes = await wasm;
    if (child.code !== null) return;
    this.log(`$ ${r.program} ${(r.args ?? []).join(' ')}`);
    const process = await JvmProcess.start(
      bytes,
      {
        args: [...program.args, ...(r.args ?? [])],
        env: { ...program.env, PWD: r.cwd ?? '/' },
        mounts: this.mounts,
        stdin: true,
      },
      {
        // Its errors in the console too: the program usually keeps them in a log file.
        onOutput: (fd, line) => fd === 2 && this.log(`${r.program}: ${line}`),
        onData: (fd, data) => {
          const pipe = child.pipes[fd === 2 ? 1 : 0];
          pipe.chunks.push(data);
          this.pump(pipe);
        },
        onExit: (code) => this.finish(child, code),
        onError: (message) => this.log(`${r.program}: ${message}`),
      },
    );
    if (child.code !== null) {
      process.kill();
      return;
    }
    child.process = process;
    for (const data of child.early) process.writeStdin(data);
    child.early = [];
  }

  private finish(child: Child, code: number) {
    if (child.code !== null) return;
    child.code = code;
    for (const pipe of child.pipes) {
      pipe.closed = true;
      this.pump(pipe);
    }
    for (const cell of child.waiters) this.reply(cell, code);
    child.waiters = [];
  }

  /** Answers a waiting read with what the pipe holds, or its end. */
  private pump(pipe: Pipe) {
    const reader = pipe.reader;
    if (!reader || (!pipe.chunks.length && !pipe.closed)) return;
    pipe.reader = null;
    const memory = new Uint8Array(this.memory.buffer);
    let at = 0;
    while (at < reader.capacity && pipe.chunks.length) {
      const chunk = pipe.chunks[0];
      const n = Math.min(chunk.length, reader.capacity - at);
      memory.set(chunk.subarray(0, n), reader.buffer + at);
      at += n;
      if (n === chunk.length) pipe.chunks.shift();
      else pipe.chunks[0] = chunk.subarray(n);
    }
    this.reply(reader.cell, at);
  }

  private reply(cell: number, value: number) {
    const cells = new Int32Array(this.memory.buffer);
    Atomics.store(cells, cell / 4, value);
    Atomics.notify(cells, cell / 4);
  }
}
