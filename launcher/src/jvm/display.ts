// The page's side of the VM's windows: shows the frames a window's thread
// presents on a canvas, reports the canvas size and focus, and sends input
// as events through the shared input block (see protocol INPUT and
// vm/crates/jvm/src/web/sdl.rs for the event layout).

import { tr } from '../i18n';
import { INPUT, type DisplayMessage } from './protocol';

/** DOM KeyboardEvent.code → SDL scancode (USB HID usage). */
const SCANCODES: Record<string, number> = {
  ...Object.fromEntries([...'ABCDEFGHIJKLMNOPQRSTUVWXYZ'].map((c, i) => [`Key${c}`, 4 + i])),
  Digit1: 30, Digit2: 31, Digit3: 32, Digit4: 33, Digit5: 34, Digit6: 35, Digit7: 36, Digit8: 37, Digit9: 38, Digit0: 39,
  Enter: 40, Escape: 41, Backspace: 42, Tab: 43, Space: 44, Minus: 45, Equal: 46, BracketLeft: 47, BracketRight: 48,
  Backslash: 49, Semicolon: 51, Quote: 52, Backquote: 53, Comma: 54, Period: 55, Slash: 56, CapsLock: 57,
  F1: 58, F2: 59, F3: 60, F4: 61, F5: 62, F6: 63, F7: 64, F8: 65, F9: 66, F10: 67, F11: 68, F12: 69,
  PrintScreen: 70, ScrollLock: 71, Pause: 72, Insert: 73, Home: 74, PageUp: 75, Delete: 76, End: 77, PageDown: 78,
  ArrowRight: 79, ArrowLeft: 80, ArrowDown: 81, ArrowUp: 82, NumLock: 83, NumpadDivide: 84, NumpadMultiply: 85,
  NumpadSubtract: 86, NumpadAdd: 87, NumpadEnter: 88, Numpad1: 89, Numpad2: 90, Numpad3: 91, Numpad4: 92, Numpad5: 93,
  Numpad6: 94, Numpad7: 95, Numpad8: 96, Numpad9: 97, Numpad0: 98, NumpadDecimal: 99, IntlBackslash: 100,
  ContextMenu: 101, NumpadEqual: 103, F13: 104, F14: 105, F15: 106,
  ControlLeft: 224, ShiftLeft: 225, AltLeft: 226, MetaLeft: 227, ControlRight: 228, ShiftRight: 229, AltRight: 230, MetaRight: 231,
};

// SDL_Keymod bits.
const MOD = { LSHIFT: 0x1, RSHIFT: 0x2, LCTRL: 0x40, RCTRL: 0x80, LALT: 0x100, RALT: 0x200, LGUI: 0x400, RGUI: 0x800, NUM: 0x1000, CAPS: 0x2000 };
const MOD_OF: Record<string, number> = {
  ShiftLeft: MOD.LSHIFT, ShiftRight: MOD.RSHIFT, ControlLeft: MOD.LCTRL, ControlRight: MOD.RCTRL,
  AltLeft: MOD.LALT, AltRight: MOD.RALT, MetaLeft: MOD.LGUI, MetaRight: MOD.RGUI,
};

/** The largest canvas side in pixels (see measure). */
const MAX_PIXELS = 4096;

/** SDL_SystemCursor → CSS cursor. */
const CURSORS = ['default', 'text', 'wait', 'crosshair', 'progress', 'nwse-resize', 'nesw-resize', 'ew-resize', 'ns-resize', 'move', 'not-allowed', 'pointer'];

export class Display {
  private canvas = document.createElement('canvas');
  private context = this.canvas.getContext('2d', { alpha: false, desynchronized: true })!;
  private input: Int32Array;
  private mods = 0;
  private relative = false;
  private hint = document.createElement('div');
  private resizeObserver: ResizeObserver;

  constructor(
    private container: HTMLElement,
    input: SharedArrayBuffer,
    private onTitle: (title: string) => void = () => {},
  ) {
    this.input = new Int32Array(input);
    this.canvas.className = 'jvm-display';
    this.canvas.tabIndex = 0;
    this.canvas.hidden = true;
    this.hint.className = 'jvm-display-hint';
    this.hint.textContent = tr('clickToCaptureMouse');
    this.hint.hidden = true;
    container.append(this.canvas, this.hint);
    this.resizeObserver = new ResizeObserver(() => this.measure());
    this.resizeObserver.observe(container);
    this.measure();
    Atomics.store(this.input, INPUT.SCREEN_WIDTH, Math.round(screen.width * devicePixelRatio));
    Atomics.store(this.input, INPUT.SCREEN_HEIGHT, Math.round(screen.height * devicePixelRatio));
    this.listen();
  }

  /** A message from a window's thread. */
  handle(message: DisplayMessage) {
    switch (message.type) {
      case 'window':
        if (message.visible) {
          this.canvas.hidden = false;
          this.canvas.focus();
          this.event(10);
        }
        break;
      case 'frame':
        // Drawn and closed at once: a frame's pixels live outside the JS heap,
        // and waiting for the garbage collector to free them leaks gigabytes.
        if (this.canvas.width !== message.bitmap.width || this.canvas.height !== message.bitmap.height) {
          this.canvas.width = message.bitmap.width;
          this.canvas.height = message.bitmap.height;
        }
        this.context.drawImage(message.bitmap, 0, 0);
        message.bitmap.close();
        Atomics.add(this.input, INPUT.FRAMES_SHOWN, 1);
        Atomics.notify(this.input, INPUT.FRAMES_SHOWN);
        break;
      case 'title':
        this.onTitle(message.title);
        break;
      case 'cursor':
        this.canvas.style.cursor = CURSORS[message.kind] ?? 'default';
        break;
      case 'relative-mouse':
        this.relative = message.enabled;
        if (message.enabled) {
          if (document.pointerLockElement !== this.canvas) this.hint.hidden = false;
        } else {
          this.hint.hidden = true;
          if (document.pointerLockElement === this.canvas) document.exitPointerLock();
        }
        break;
      case 'clipboard':
        void navigator.clipboard?.writeText(message.text).catch(() => {});
        break;
      case 'open-url':
        if (/^https?:/.test(message.url)) open(message.url, '_blank', 'noopener');
        break;
      case 'window-destroyed':
      case 'audio':
        break;
    }
  }

  private measure() {
    const rect = this.container.getBoundingClientRect();
    const dpr = devicePixelRatio || 1;
    const width = Math.max(1, Math.round(rect.width));
    const height = Math.max(1, Math.round(rect.height));
    // WebGL caps drawing buffers (commonly at 4096 per side): a larger canvas
    // gets a smaller buffer, and the game would draw for a size it lacks.
    const scale = Math.min(dpr, MAX_PIXELS / width, MAX_PIXELS / height);
    const pw = Math.max(1, Math.round(width * scale));
    const ph = Math.max(1, Math.round(height * scale));
    const changed = Atomics.load(this.input, INPUT.PIXEL_WIDTH) !== pw || Atomics.load(this.input, INPUT.PIXEL_HEIGHT) !== ph;
    Atomics.store(this.input, INPUT.CSS_WIDTH, width);
    Atomics.store(this.input, INPUT.CSS_HEIGHT, height);
    Atomics.store(this.input, INPUT.PIXEL_WIDTH, pw);
    Atomics.store(this.input, INPUT.PIXEL_HEIGHT, ph);
    Atomics.store(this.input, INPUT.DPR, Math.round(dpr * 1000));
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${height}px`;
    if (changed) this.event(6, width, height, pw, ph);
  }

  /** Appends an event to the ring (kind, then up to 7 words). */
  private event(kind: number, ...words: number[]) {
    const written = Atomics.load(this.input, INPUT.WRITTEN);
    if (written - Atomics.load(this.input, INPUT.READ) >= INPUT.EVENTS) return; // full: drop
    const slot = INPUT.RING + (written % INPUT.EVENTS) * INPUT.EVENT_INTS;
    this.input.fill(0, slot, slot + INPUT.EVENT_INTS);
    this.input[slot] = kind;
    words.forEach((w, i) => (this.input[slot + 1 + i] = w | 0));
    Atomics.store(this.input, INPUT.WRITTEN, written + 1);
  }

  private bits(f: number) {
    const view = new DataView(new ArrayBuffer(4));
    view.setFloat32(0, f, true);
    return view.getInt32(0, true);
  }

  private position(e: MouseEvent): [number, number] {
    const rect = this.canvas.getBoundingClientRect();
    return [e.clientX - rect.left, e.clientY - rect.top];
  }

  private buttons(e: MouseEvent) {
    // DOM buttons: 1 left, 2 right, 4 middle, 8 back, 16 forward → SDL bits
    // (1 left, 2 middle, 4 right, 8 X1, 16 X2).
    const b = e.buttons;
    return (b & 1) | ((b & 4) >> 1) | ((b & 2) << 1) | (b & 8) | (b & 16);
  }

  private listen() {
    const canvas = this.canvas;
    const key = (e: KeyboardEvent, down: boolean) => {
      const scancode = SCANCODES[e.code];
      if (scancode === undefined) return;
      const bit = MOD_OF[e.code] ?? 0;
      this.mods = down ? this.mods | bit : this.mods & ~bit;
      const mods = this.mods | (e.getModifierState('CapsLock') ? MOD.CAPS : 0) | (e.getModifierState('NumLock') ? MOD.NUM : 0);
      this.event(1, scancode, down ? 1 : 0, e.repeat ? 1 : 0, mods);
      if (down && e.key.length === 1 && !e.ctrlKey && !e.metaKey) this.text(e.key);
      // Keep keys for the game, except the browser's own reload/devtools/fullscreen.
      if (!['F5', 'F11', 'F12'].includes(e.code) && !(e.ctrlKey && ['KeyR', 'KeyW', 'KeyT'].includes(e.code))) e.preventDefault();
    };
    canvas.addEventListener('keydown', (e) => key(e, true));
    canvas.addEventListener('keyup', (e) => key(e, false));
    canvas.addEventListener('mousemove', (e) => {
      const [x, y] = this.position(e);
      this.event(3, this.bits(x), this.bits(y), this.bits(e.movementX), this.bits(e.movementY), this.buttons(e));
    });
    const button = (e: MouseEvent, down: boolean) => {
      // DOM 0 left, 1 middle, 2 right, 3 back, 4 forward: SDL's are one more.
      const [x, y] = this.position(e);
      this.event(4, e.button + 1, down ? 1 : 0, Math.max(1, e.detail), this.bits(x), this.bits(y));
      e.preventDefault();
    };
    canvas.addEventListener('mousedown', (e) => {
      canvas.focus();
      if (this.relative && document.pointerLockElement !== canvas) void canvas.requestPointerLock();
      button(e, true);
    });
    canvas.addEventListener('mouseup', (e) => button(e, false));
    canvas.addEventListener('contextmenu', (e) => e.preventDefault());
    canvas.addEventListener(
      'wheel',
      (e) => {
        const [x, y] = this.position(e);
        const scale = e.deltaMode === 1 ? 1 : e.deltaMode === 2 ? 3 : 1 / 100;
        this.event(5, this.bits(-e.deltaX * scale), this.bits(-e.deltaY * scale), this.bits(x), this.bits(y));
        e.preventDefault();
      },
      { passive: false },
    );
    canvas.addEventListener('focus', () => {
      Atomics.store(this.input, INPUT.FOCUSED, 1);
      this.event(7, 1);
    });
    canvas.addEventListener('blur', () => {
      Atomics.store(this.input, INPUT.FOCUSED, 0);
      this.mods = 0;
      this.event(7, 0);
    });
    canvas.addEventListener('mouseenter', () => this.event(8, 1));
    canvas.addEventListener('mouseleave', () => this.event(8, 0));
    document.addEventListener('pointerlockchange', () => {
      const locked = document.pointerLockElement === canvas;
      Atomics.store(this.input, INPUT.LOCKED, locked ? 1 : 0);
      this.hint.hidden = locked || !this.relative;
    });
  }

  private text(text: string) {
    const bytes = new TextEncoder().encode(text).subarray(0, 27);
    const padded = new Uint8Array(28);
    padded.set(bytes);
    const words = new Int32Array(padded.buffer);
    this.event(2, ...words);
  }
}
