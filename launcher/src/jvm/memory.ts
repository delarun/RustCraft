// Typed views of the VM's shared memory, renewed when it grows.

export class Memory {
  private buffer: SharedArrayBuffer | null = null;
  private views!: {
    u8: Uint8Array;
    i8: Int8Array;
    u16: Uint16Array;
    i16: Int16Array;
    u32: Uint32Array;
    i32: Int32Array;
    f32: Float32Array;
    f64: Float64Array;
  };

  constructor(private memory: WebAssembly.Memory) {}

  private get v() {
    const buffer = this.memory.buffer as unknown as SharedArrayBuffer;
    if (buffer !== this.buffer) {
      this.buffer = buffer;
      this.views = {
        u8: new Uint8Array(buffer),
        i8: new Int8Array(buffer),
        u16: new Uint16Array(buffer),
        i16: new Int16Array(buffer),
        u32: new Uint32Array(buffer),
        i32: new Int32Array(buffer),
        f32: new Float32Array(buffer),
        f64: new Float64Array(buffer),
      };
    }
    return this.views;
  }

  get u8() {
    return this.v.u8;
  }
  get i8() {
    return this.v.i8;
  }
  get u16() {
    return this.v.u16;
  }
  get i16() {
    return this.v.i16;
  }
  get u32() {
    return this.v.u32;
  }
  get i32() {
    return this.v.i32;
  }
  get f32() {
    return this.v.f32;
  }
  get f64() {
    return this.v.f64;
  }
}
