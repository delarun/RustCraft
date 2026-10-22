// Development console for the JVM (jvm.html): runs rjava.wasm with the
// arguments and mounts given to window.runJvm (see e2e/jvm.mjs) and shows its
// output. Lines are also logged to the browser console as `[fd] line`.

import { startAudio } from './audio';
import { Display } from './display';
import { JvmProcess } from './process';
import { INPUT_BYTES, type ProcessConfig } from './protocol';

export interface RunRequest extends ProcessConfig {
  /** URL of rjava.wasm. */
  wasm: string;
}

const output = document.getElementById('console')!;

function show(text: string, stderr: boolean) {
  const line = document.createElement('span');
  if (stderr) line.className = 'stderr';
  line.textContent = `${text}\n`;
  output.append(line);
}

async function runJvm(request: RunRequest): Promise<number> {
  const wasm = await (await fetch(request.wasm)).arrayBuffer();
  const input = new SharedArrayBuffer(INPUT_BYTES);
  let process: JvmProcess | null = null;
  const display = new Display(document.getElementById('screen')!, input, (title) => (document.title = title));
  return new Promise((resolve, reject) => {
    JvmProcess.start(wasm, request, {
      onDisplay(message) {
        if (message.type === 'audio') void startAudio(process!.memory, message.ring).catch((e) => console.error('audio:', e));
        else display.handle(message);
      },
      onOutput(fd, line) {
        show(line, fd === 2);
        console.log(`[${fd}] ${line}`);
      },
      onExit(code) {
        show(`— exit ${code}`, code !== 0);
        resolve(code);
      },
      onError(message) {
        show(message, true);
        console.error(message);
      },
    }, input)
      .then((started) => (process = started))
      .catch(reject);
  });
}

declare global {
  interface Window {
    runJvm: typeof runJvm;
  }
}
window.runJvm = runJvm;

// jvm.html?run=<url of a RunRequest JSON>: runs it at once, full-window.
const preset = new URLSearchParams(location.search).get('run');
if (preset) {
  document.body.classList.add('play');
  void fetch(preset)
    .then((r) => r.json() as Promise<RunRequest>)
    .then((request) => runJvm({ ...request, wasm: new URL(request.wasm, location.href).href }))
    .catch((e) => show(String(e), true));
}
