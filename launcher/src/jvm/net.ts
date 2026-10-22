// TCP connections of the VM (Minecraft servers), through rustcraft-bridge: a
// program on the player's computer (bridge/) that joins WebSockets from this
// page to TCP connections. Runs on the page's main thread, which keeps an
// event loop for the WebSockets while the VM's threads block in wasm. Each
// connection's data passes through two rings in the VM's memory, laid out by
// vm/crates/jvm/src/web/net.rs (`Shared`).

/** The bridge's default address (bridge/src/main.rs). */
export const DEFAULT_BRIDGE = 'ws://127.0.0.1:25580';

// `Shared` fields, as Int32 indices from the block.
const STATE = 0;
const RX_HEAD = 1;
const RX_TAIL = 2;
const TX_HEAD = 3;
const TX_TAIL = 4;
const CLOSING = 5;
const RX_WAITING = 6;
const RX = 7;
const RX_SIZE = 8;
const TX = 9;
const TX_SIZE = 10;

const OPEN = 1;
const CLOSED = 2;
const FAILED = 3;
const RELEASED = 4;

interface Connection {
  socket: WebSocket;
  /** Received data that did not fit the ring yet. */
  pending: Uint8Array[];
  opened: boolean;
}

export class NetBridge {
  private connections = new Map<number, Connection>();

  constructor(
    private memory: WebAssembly.Memory,
    /** ws://host:port of rustcraft-bridge. */
    private base = DEFAULT_BRIDGE,
  ) {}

  private words(block: number) {
    return new Int32Array(this.memory.buffer, block, 11);
  }

  /** A connection to `address` (host:port) for the block at `block`. */
  open(block: number, address: string) {
    const words = this.words(block);
    const split = /^\[?([^\]]*)\]?:(\d+)$/.exec(address);
    let socket: WebSocket;
    try {
      if (!split) throw new Error(`bad address ${address}`);
      const url = `${this.base.replace(/\/$/, '')}/connect?host=${encodeURIComponent(split[1])}&port=${split[2]}`;
      socket = new WebSocket(url);
    } catch (error) {
      console.warn('net:', error);
      Atomics.store(words, STATE, FAILED);
      return;
    }
    socket.binaryType = 'arraybuffer';
    const connection: Connection = { socket, pending: [], opened: false };
    this.connections.set(block, connection);
    socket.onopen = () => {
      connection.opened = true;
      Atomics.store(words, STATE, OPEN);
      this.kick(block);
    };
    socket.onmessage = (event) => {
      connection.pending.push(new Uint8Array(event.data as ArrayBuffer));
      this.receive(block, connection);
    };
    socket.onclose = () => {
      // Closed by the server: the data received so far is still read.
      this.receive(block, connection);
      Atomics.store(words, STATE, connection.opened ? CLOSED : FAILED);
      if (Atomics.load(words, CLOSING)) this.release(block);
    };
    socket.onerror = () => {
      if (!connection.opened) Atomics.store(words, STATE, FAILED);
    };
  }

  /** The VM sent data, closed, or made room in the receive ring. */
  kick(block: number) {
    const connection = this.connections.get(block);
    if (!connection) return;
    const words = this.words(block);
    this.send(words, connection);
    this.receive(block, connection);
    if (Atomics.load(words, CLOSING)) {
      if (connection.socket.readyState === WebSocket.OPEN || connection.socket.readyState === WebSocket.CONNECTING) {
        connection.socket.close();
      }
      this.release(block);
    }
  }

  /** Data from the send ring to the WebSocket. */
  private send(words: Int32Array, connection: Connection) {
    if (connection.socket.readyState !== WebSocket.OPEN) return;
    const head = Atomics.load(words, TX_HEAD) >>> 0;
    const tail = Atomics.load(words, TX_TAIL) >>> 0;
    const length = (head - tail) >>> 0;
    if (length === 0) return;
    const ring = words[TX] >>> 0;
    const size = words[TX_SIZE] >>> 0;
    const bytes = new Uint8Array(this.memory.buffer);
    const start = tail & (size - 1);
    const out = new Uint8Array(length);
    const first = Math.min(length, size - start);
    out.set(bytes.subarray(ring + start, ring + start + first));
    if (first < length) out.set(bytes.subarray(ring, ring + length - first), first);
    connection.socket.send(out);
    Atomics.store(words, TX_TAIL, (tail + length) | 0);
  }

  /** Pending received data into the receive ring, as far as it fits. */
  private receive(block: number, connection: Connection) {
    if (!this.connections.has(block)) return;
    const words = this.words(block);
    const ring = words[RX] >>> 0;
    const size = words[RX_SIZE] >>> 0;
    const bytes = new Uint8Array(this.memory.buffer);
    let head = Atomics.load(words, RX_HEAD) >>> 0;
    while (connection.pending.length) {
      const tail = Atomics.load(words, RX_TAIL) >>> 0;
      const room = size - ((head - tail) >>> 0);
      if (room === 0) break;
      const chunk = connection.pending[0];
      const n = Math.min(room, chunk.length);
      for (let i = 0; i < n; ) {
        const at = (head + i) & (size - 1);
        const run = Math.min(n - i, size - at);
        bytes.set(chunk.subarray(i, i + run), ring + at);
        i += run;
      }
      head = (head + n) >>> 0;
      Atomics.store(words, RX_HEAD, head | 0);
      if (n === chunk.length) connection.pending.shift();
      else connection.pending[0] = chunk.subarray(n);
    }
    // Full: the VM kicks once it has read some.
    Atomics.store(words, RX_WAITING, connection.pending.length ? 1 : 0);
  }

  private release(block: number) {
    const connection = this.connections.get(block);
    if (connection) {
      connection.socket.onopen = connection.socket.onmessage = connection.socket.onclose = connection.socket.onerror = null;
    }
    this.connections.delete(block);
    Atomics.store(this.words(block), STATE, RELEASED);
  }
}

/** Whether rustcraft-bridge answers at `base` (its /status). */
export async function bridgeStatus(base = DEFAULT_BRIDGE): Promise<{ version: string } | null> {
  try {
    const url = `${base.replace(/^ws/, 'http').replace(/\/$/, '')}/status`;
    const response = await fetch(url, { mode: 'cors', cache: 'no-store', signal: AbortSignal.timeout(2000) });
    if (!response.ok) return null;
    const body = (await response.json()) as { name?: string; version?: string };
    return body.name === 'rustcraft-bridge' ? { version: body.version ?? '?' } : null;
  } catch {
    return null;
  }
}
