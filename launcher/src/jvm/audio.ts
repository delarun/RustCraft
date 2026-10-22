// The page's audio: an AudioContext playing the VM's OpenAL ring through an
// AudioWorklet. Browsers start audio only after a user gesture, so the
// context resumes on the first click or key press.

import workletUrl from './audio.worklet.ts?worker&url';

export async function startAudio(memory: WebAssembly.Memory, ring: number): Promise<AudioContext> {
  const context = new AudioContext({ sampleRate: 48_000, latencyHint: 'interactive' });
  await context.audioWorklet.addModule(workletUrl);
  const node = new AudioWorkletNode(context, 'rjava-ring', {
    numberOfInputs: 0,
    outputChannelCount: [2],
    processorOptions: { buffer: memory.buffer, ring },
  });
  node.connect(context.destination);
  if (context.state !== 'running') {
    const resume = () => void context.resume();
    addEventListener('pointerdown', resume, { once: true, capture: true });
    addEventListener('keydown', resume, { once: true, capture: true });
  }
  return context;
}
