// Plays the VM's OpenAL output: a ring of stereo f32 frames in the VM's
// memory (see vm/crates/jvm/src/web/al.rs, struct Ring).

interface AudioWorkletProcessorType {
  readonly port: MessagePort;
}
declare const AudioWorkletProcessor: { prototype: AudioWorkletProcessorType; new (): AudioWorkletProcessorType };
declare function registerProcessor(name: string, processor: new (options: { processorOptions: RingOptions }) => unknown): void;

interface RingOptions {
  buffer: SharedArrayBuffer;
  ring: number;
}

class RingPlayer extends AudioWorkletProcessor {
  private header: Int32Array;
  private samples: Float32Array;
  private capacity: number;

  constructor(options: { processorOptions: RingOptions }) {
    super();
    const { buffer, ring } = options.processorOptions;
    this.header = new Int32Array(buffer, ring, 4);
    this.capacity = this.header[2];
    this.samples = new Float32Array(buffer, ring + 16, this.capacity * 2);
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]) {
    const [left, right] = outputs[0];
    const written = Atomics.load(this.header, 0);
    let read = Atomics.load(this.header, 1);
    const available = (written - read) | 0;
    for (let i = 0; i < left.length; i++) {
      if (i < available) {
        const at = ((read + i) >>> 0) % this.capacity;
        left[i] = this.samples[at * 2];
        if (right) right[i] = this.samples[at * 2 + 1];
      } else {
        left[i] = 0;
        if (right) right[i] = 0;
      }
    }
    read = (read + Math.min(available, left.length)) | 0;
    Atomics.store(this.header, 1, read);
    return true;
  }
}

registerProcessor('rjava-ring', RingPlayer);
