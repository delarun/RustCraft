// The VM's platform imports in a thread worker (`rjava_web`, `rjava_gl`):
// windows are OffscreenCanvases with WebGL2 contexts owned by the thread that
// creates them; a presented frame goes to the page as an ImageBitmap. Input
// comes from the page through the shared input block.

import { GlEmulation } from './gl';
import { Memory } from './memory';
import { INPUT, type DisplayMessage, type NetMessage } from './protocol';

/** Frames sent but not yet shown that a presenting thread waits for. */
const MAX_FRAMES_IN_FLIGHT = 2;

export interface PlatformHost {
  memory: WebAssembly.Memory;
  input: SharedArrayBuffer;
  post(message: DisplayMessage | NetMessage, transfer?: Transferable[]): void;
  /** malloc/free in the VM's memory (set once the instance exists). */
  malloc(size: number): number;
  free(pointer: number): void;
  /** Check every GL call for errors (RJAVA_GL_DEBUG). */
  debugGl?: boolean;
  /** Log frames per second (RJAVA_WEB_STATS). */
  stats?: boolean;
  /** Present without suspending (RJAVA_NO_JSPI: for the profiler, which does
   *  not see suspendable stacks; leaks frames). */
  noJspi?: boolean;
}

/** Resolves in a new task of this worker's event loop. */
function nextTask(): Promise<void> {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => resolve();
    channel.port2.postMessage(0);
  });
}

export function platformImports(host: PlatformHost) {
  const memory = new Memory(host.memory);
  const input = new Int32Array(host.input);
  /** Windows (visible or not) and the contexts' canvases and target windows. */
  const windows = new Map<number, { visible: boolean }>();
  const contexts = new Map<number, { canvas: OffscreenCanvas; window: number }>();
  let nextContext = 1;
  const gl = new GlEmulation(memory, (n) => host.malloc(n), (p) => host.free(p));
  let framesSent = 0;
  let statsSince = performance.now();
  let statsFrames = 0;
  const decoder = new TextDecoder();
  const text = (pointer: number, length: number) => decoder.decode(memory.u8.slice(pointer >>> 0, (pointer >>> 0) + length));

  /** Sizes a window's canvas as the page shows it. */
  const fit = (canvas: OffscreenCanvas) => {
    const width = Math.max(1, input[INPUT.PIXEL_WIDTH]);
    const height = Math.max(1, input[INPUT.PIXEL_HEIGHT]);
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
    }
  };

  const web = {
    window_create(id: number, width: number, height: number, visible: number) {
      windows.set(id, { visible: !!visible });
      host.post({ type: 'window', id, width, height, visible: !!visible });
    },
    window_show(id: number, visible: number) {
      const window = windows.get(id);
      if (window) window.visible = !!visible;
      for (const context of contexts.values()) if (context.window === id && visible) fit(context.canvas);
      host.post({ type: 'window', id, width: 0, height: 0, visible: !!visible });
    },
    window_destroy(id: number) {
      windows.delete(id);
      host.post({ type: 'window-destroyed', id });
    },
    window_title(id: number, pointer: number, length: number) {
      host.post({ type: 'title', id, title: text(pointer, length) });
    },
    page_state(out: number) {
      const i32 = memory.i32;
      const at = out >>> 2;
      const fields = [
        INPUT.CSS_WIDTH,
        INPUT.CSS_HEIGHT,
        INPUT.PIXEL_WIDTH,
        INPUT.PIXEL_HEIGHT,
        INPUT.DPR,
        INPUT.FOCUSED,
        INPUT.LOCKED,
        INPUT.SCREEN_WIDTH,
        INPUT.SCREEN_HEIGHT,
      ];
      fields.forEach((field, i) => (i32[at + i] = Atomics.load(input, field)));
    },
    event_next(out: number) {
      const read = Atomics.load(input, INPUT.READ);
      if (read === Atomics.load(input, INPUT.WRITTEN)) return 0;
      const slot = INPUT.RING + (read % INPUT.EVENTS) * INPUT.EVENT_INTS;
      memory.i32.set(input.subarray(slot, slot + INPUT.EVENT_INTS), out >>> 2);
      Atomics.store(input, INPUT.READ, read + 1);
      return 1;
    },
    gl_context_create(window: number) {
      const canvas = new OffscreenCanvas(1, 1);
      if (windows.get(window)?.visible) fit(canvas);
      const id = nextContext++;
      if (!gl.createContext(id, canvas)) return 0;
      contexts.set(id, { canvas, window });
      return id;
    },
    gl_make_current(window: number, context: number) {
      const target = contexts.get(context);
      if (target) {
        target.window = window;
        if (windows.get(window)?.visible) fit(target.canvas);
      }
      gl.makeCurrent(context);
    },
    window_present(id: number) {
      const canvas = [...contexts.values()].find((c) => c.window === id)?.canvas;
      if (!canvas) return;
      // Pace to the page: wait while too many frames are on their way.
      while (framesSent - Atomics.load(input, INPUT.FRAMES_SHOWN) >= MAX_FRAMES_IN_FLIGHT) {
        if (Atomics.wait(input, INPUT.FRAMES_SHOWN, Atomics.load(input, INPUT.FRAMES_SHOWN), 100) === 'timed-out') break;
      }
      if (host.stats) {
        const now = performance.now();
        if (now - statsSince >= 10_000) {
          console.log(`[stats] ${((framesSent - statsFrames) * 1000 / (now - statsSince)).toFixed(1)} fps`);
          statsSince = now;
          statsFrames = framesSent;
        }
      }
      const bitmap = canvas.transferToImageBitmap();
      framesSent++;
      host.post({ type: 'frame', id, bitmap }, [bitmap]);
      fit(canvas);
    },
    cursor_set(kind: number) {
      host.post({ type: 'cursor', kind });
    },
    mouse_relative(enabled: number) {
      host.post({ type: 'relative-mouse', enabled: !!enabled });
    },
    audio_start(ring: number) {
      host.post({ type: 'audio', ring: ring >>> 0 });
    },
    net_open(block: number, pointer: number, length: number) {
      host.post({ type: 'net-open', block: block >>> 0, address: text(pointer, length) });
    },
    net_kick(block: number) {
      host.post({ type: 'net-kick', block: block >>> 0 });
    },
    open_url(pointer: number, length: number) {
      host.post({ type: 'open-url', url: text(pointer, length) });
    },
    clipboard_set(pointer: number, length: number) {
      host.post({ type: 'clipboard', text: text(pointer, length) });
    },
  };

  // Presenting suspends the thread (JSPI) for a turn of this worker's event
  // loop: the browser frees transferred frames' buffers in tasks there, and a
  // thread that never returns to it leaks every frame.
  const present = web.window_present;
  const suspending = {
    ...web,
    window_present: new WebAssembly.Suspending(async (id: number) => {
      present(id);
      await nextTask();
    }),
  };
  return { rjava_web: host.noJspi ? web : suspending, rjava_gl: gl.imports(host.debugGl) };
}
