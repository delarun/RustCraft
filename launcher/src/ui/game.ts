// Runs the game in the page: the VM's window fills the viewport; its output
// shows over the screen until the first frame.

import { startAudio } from '../jvm/audio';
import { Display } from '../jvm/display';
import { JvmProcess } from '../jvm/process';
import { INPUT_BYTES, type ProcessConfig } from '../jvm/protocol';
import { tr } from '../i18n';
import { $ } from './dom';

/** Our VM, built from vm/ (served by the dev server; copied next to the page in a deployment). */
export const RJAVA_WASM_URL = (import.meta.env.VITE_RJAVA_WASM as string | undefined) ?? `${import.meta.env.BASE_URL}rjava.wasm`;

const MAX_CONSOLE_LINES = 2000;

export interface GameCallbacks {
  onStatus(message: string): void;
  onExit(code: number): void;
}

export async function runGame(config: ProcessConfig, callbacks: GameCallbacks): Promise<JvmProcess> {
  callbacks.onStatus(tr('loadingJvm'));
  const response = await fetch(RJAVA_WASM_URL);
  if (!response.ok) throw new Error(`rjava.wasm: HTTP ${response.status}`);
  const wasm = await response.arrayBuffer();

  const game = $('game');
  const screen = $('screen');
  const output = $('console');
  const stop = $<HTMLButtonElement>('stop');
  output.replaceChildren();
  game.hidden = false;
  document.body.classList.add('playing');

  const input = new SharedArrayBuffer(INPUT_BYTES);
  const display = new Display(screen, input, (title) => (document.title = title));
  const show = (text: string, stderr: boolean) => {
    const line = document.createElement('span');
    if (stderr) line.className = 'stderr';
    line.textContent = `${text}\n`;
    output.append(line);
    while (output.childElementCount > MAX_CONSOLE_LINES) output.firstElementChild?.remove();
    output.scrollTop = output.scrollHeight;
  };

  let process: JvmProcess | null = null;
  const finish = (code: number) => {
    document.body.classList.add('exited');
    show(tr('jvmExited', code), code !== 0);
    callbacks.onExit(code);
  };
  stop.onclick = () => {
    if (!process) return;
    if (!document.body.classList.contains('exited') && !confirm(tr('confirmStop'))) return;
    process.kill();
    // The display keeps listeners on the page: start over from a clean page.
    location.reload();
  };

  process = await JvmProcess.start(
    wasm,
    config,
    {
      onDisplay(message) {
        if (message.type === 'audio') void startAudio(process!.memory, message.ring).catch((e) => console.error('audio:', e));
        else display.handle(message);
      },
      onOutput(fd, line) {
        show(line, fd === 2);
        console.log(`[${fd}] ${line}`);
      },
      onExit: finish,
      onError(message) {
        show(message, true);
        console.error(message);
      },
    },
    input,
  );
  callbacks.onStatus(tr('gameStarted'));
  return process;
}
