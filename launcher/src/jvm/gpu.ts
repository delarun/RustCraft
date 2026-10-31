// WebGPU for a program's wgpu (Horizon: crates/horizon-web/src/gpu.rs), the
// `rc_gpu` imports. Runs in the thread that renders: objects live in this
// worker and are named by numbers. Descriptors arrive as JSON in WebGPU's
// shape with objects as numbers; a pass arrives whole, as recorded commands.
// Waiting (adapters, devices, mapping, error scopes, presenting) suspends the
// thread (JSPI).

import type { Memory } from './memory';

export interface GpuHost {
  memory: Memory;
  /** Shows what `canvas` holds as window `window`'s frame (platform.ts). */
  present(window: number, canvas: OffscreenCanvas): void;
  /** Frames drawn into a texture and read back instead of a WebGPU canvas
   *  (RJAVA_GPU_READBACK: headless Chrome's SwiftShader loses the device
   *  when a canvas texture is drawn into). */
  readback?: boolean;
}

/** Resolves in a new task of this worker's event loop. */
function nextTask(): Promise<void> {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => resolve();
    channel.port2.postMessage(0);
  });
}

/** Resolves at the display's next refresh (a worker's requestAnimationFrame
 *  follows the page's), else in a new task. */
function nextFrame(): Promise<void> {
  const raf = (self as { requestAnimationFrame?: (callback: () => void) => number }).requestAnimationFrame;
  return raf ? new Promise((resolve) => raf(() => resolve())) : nextTask();
}

/** Drops null fields of objects (WebGPU reads null as a value), recursively. */
function clean<T>(value: T): T {
  if (Array.isArray(value)) return value.map(clean) as T;
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [key, v] of Object.entries(value)) if (v !== null) out[key] = clean(v);
    return out as T;
  }
  return value;
}

// Pass commands (gpu.rs `Ops`): an opcode, then its words.
const OP = {
  SET_PIPELINE: 1,
  SET_BIND_GROUP: 2,
  SET_INDEX_BUFFER: 3,
  SET_VERTEX_BUFFER: 4,
  SET_BLEND_CONSTANT: 5,
  SET_SCISSOR_RECT: 6,
  SET_VIEWPORT: 7,
  SET_STENCIL_REFERENCE: 8,
  DRAW: 9,
  DRAW_INDEXED: 10,
  DRAW_INDIRECT: 11,
  DRAW_INDEXED_INDIRECT: 12,
  EXECUTE_BUNDLES: 13,
  BEGIN_OCCLUSION_QUERY: 14,
  END_OCCLUSION_QUERY: 15,
  DISPATCH: 16,
  DISPATCH_INDIRECT: 17,
} as const;

// Object kinds (gpu.rs `Kind`).
const KIND = {
  BUFFER: 1,
  TEXTURE: 2,
  TEXTURE_VIEW: 3,
  SAMPLER: 4,
  BIND_GROUP_LAYOUT: 5,
  BIND_GROUP: 6,
  PIPELINE_LAYOUT: 7,
  SHADER_MODULE: 8,
  RENDER_PIPELINE: 9,
  COMPUTE_PIPELINE: 10,
  COMMAND_ENCODER: 11,
  QUERY_SET: 12,
} as const;

interface Surface {
  canvas: OffscreenCanvas;
  context: GPUCanvasContext;
  window: number;
  /** Read-back frames: the texture drawn into and the device. */
  texture?: GPUTexture;
  device?: GPUDevice;
}

/** A buffer's mapping: its range, and the bytes once read (one getMappedRange). */
interface Mapping {
  offset: number;
  size: number;
  bytes?: ArrayBuffer;
}

// GPUTextureUsage and GPUBufferUsage bits.
const RENDER_ATTACHMENT = 0x10;
const COPY_SRC = 0x01;
const COPY_DST = 0x08;
const MAP_READ = 0x01;

type AnyPass = GPURenderPassEncoder | GPUComputePassEncoder | GPURenderBundleEncoder;

export function gpuImports(host: GpuHost) {
  const { memory } = host;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const objects = new Map<number, any>();
  let nextId = 1;
  const add = (object: unknown) => {
    const id = nextId++;
    objects.set(id, object);
    return id;
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const get = (id: number): any => {
    const object = objects.get(id);
    if (object === undefined && id !== 0) throw new Error(`rc_gpu: no object ${id}`);
    return object;
  };
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  const text = (pointer: number, length: number) => decoder.decode(memory.u8.slice(pointer >>> 0, (pointer >>> 0) + length));
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const json = (pointer: number, length: number): any => clean(JSON.parse(text(pointer, length)));
  /** Writes what fits of `s`; returns its whole length (the caller asks again with more room). */
  const writeString = (s: string, out: number, cap: number) => {
    const bytes = encoder.encode(s);
    memory.u8.set(bytes.subarray(0, Math.min(cap, bytes.length)), out >>> 0);
    return bytes.length;
  };

  let lastError = '';
  /** Errors raised outside an error scope, by device. */
  const uncaptured = new Map<GPUDevice, string[]>();
  const tickets = new Map<number, { state: number; promise: Promise<void> }>();
  let nextTicket = 1;
  const track = (promise: Promise<unknown>) => {
    const ticket = nextTicket++;
    const entry = { state: 0, promise: Promise.resolve() };
    entry.promise = promise.then(
      () => void (entry.state = 1),
      (e) => {
        entry.state = 2;
        console.error('rc_gpu:', e);
      },
    );
    tickets.set(ticket, entry);
    return ticket;
  };
  const mappings = new Map<GPUBuffer, Mapping>();
  const mapped = (buffer: GPUBuffer) => {
    const mapping = mappings.get(buffer) ?? { offset: 0, size: buffer.size };
    mappings.set(buffer, mapping);
    return mapping;
  };
  /** Copies of the program's memory where WebGPU won't read shared memory. */
  let copyShared = false;
  const data = (pointer: number, length: number) => {
    const view = memory.u8.subarray(pointer >>> 0, (pointer >>> 0) + length);
    return copyShared ? view.slice() : view;
  };

  const resolveBindGroup = (desc: GPUBindGroupDescriptor & { layout: number }) => ({
    ...desc,
    layout: get(desc.layout),
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    entries: (desc.entries as any[]).map((e) => {
      const r = e.resource;
      if ('buffer' in r) return { binding: e.binding, resource: { ...r, buffer: get(r.buffer) } };
      if ('sampler' in r) return { binding: e.binding, resource: get(r.sampler) };
      return { binding: e.binding, resource: get(r.view) };
    }),
  });
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const resolveStage = (stage: any) => (stage ? { ...stage, module: get(stage.module) } : stage);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const resolveLayout = (layout: any) => (layout === 'auto' ? 'auto' : get(layout));

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const create = (parent: number, kind: number, desc: any) => {
    const owner = get(parent);
    switch (kind) {
      case KIND.BUFFER:
        return (owner as GPUDevice).createBuffer(desc);
      case KIND.TEXTURE:
        return (owner as GPUDevice).createTexture(desc);
      case KIND.TEXTURE_VIEW:
        return (owner as GPUTexture).createView(desc);
      case KIND.SAMPLER:
        return (owner as GPUDevice).createSampler(desc);
      case KIND.BIND_GROUP_LAYOUT:
        return (owner as GPUDevice).createBindGroupLayout(desc);
      case KIND.BIND_GROUP:
        return (owner as GPUDevice).createBindGroup(resolveBindGroup(desc));
      case KIND.PIPELINE_LAYOUT:
        return (owner as GPUDevice).createPipelineLayout({
          ...desc,
          bindGroupLayouts: desc.bindGroupLayouts.map((l: number | null) => (l === null ? null : get(l))),
        });
      case KIND.SHADER_MODULE:
        return (owner as GPUDevice).createShaderModule(desc);
      case KIND.RENDER_PIPELINE:
        return (owner as GPUDevice).createRenderPipeline({
          ...desc,
          layout: resolveLayout(desc.layout),
          vertex: resolveStage(desc.vertex),
          fragment: resolveStage(desc.fragment),
        });
      case KIND.COMPUTE_PIPELINE:
        return (owner as GPUDevice).createComputePipeline({
          ...desc,
          layout: resolveLayout(desc.layout),
          compute: resolveStage(desc.compute),
        });
      case KIND.COMMAND_ENCODER:
        return (owner as GPUDevice).createCommandEncoder(desc);
      case KIND.QUERY_SET:
        return (owner as GPUDevice).createQuerySet(desc);
    }
    throw new Error(`rc_gpu: no object kind ${kind}`);
  };

  /** Replays a pass's commands (gpu.rs `Ops`). */
  const replay = (pass: AnyPass, pointer: number, count: number) => {
    const words = memory.u32.slice(pointer >>> 2, (pointer >>> 2) + count);
    const floats = new Float32Array(words.buffer);
    const u64 = (at: number) => words[at] + words[at + 1] * 2 ** 32;
    const render = pass as GPURenderPassEncoder;
    let i = 0;
    while (i < words.length) {
      const op = words[i++];
      switch (op) {
        case OP.SET_PIPELINE:
          (pass as GPURenderPassEncoder).setPipeline(get(words[i++]));
          break;
        case OP.SET_BIND_GROUP: {
          const index = words[i];
          const group = words[i + 1];
          const n = words[i + 2];
          pass.setBindGroup(index, group ? get(group) : null, words.subarray(i + 3, i + 3 + n), 0, n);
          i += 3 + n;
          break;
        }
        case OP.SET_INDEX_BUFFER: {
          const [buffer, format, sized] = [words[i], words[i + 1], words[i + 2]];
          (pass as GPURenderPassEncoder).setIndexBuffer(get(buffer), format ? 'uint32' : 'uint16', u64(i + 3), sized ? u64(i + 5) : undefined);
          i += 7;
          break;
        }
        case OP.SET_VERTEX_BUFFER: {
          const [slot, buffer, sized] = [words[i], words[i + 1], words[i + 2]];
          (pass as GPURenderPassEncoder).setVertexBuffer(slot, buffer ? get(buffer) : null, u64(i + 3), sized ? u64(i + 5) : undefined);
          i += 7;
          break;
        }
        case OP.SET_BLEND_CONSTANT:
          render.setBlendConstant([floats[i], floats[i + 1], floats[i + 2], floats[i + 3]]);
          i += 4;
          break;
        case OP.SET_SCISSOR_RECT:
          render.setScissorRect(words[i], words[i + 1], words[i + 2], words[i + 3]);
          i += 4;
          break;
        case OP.SET_VIEWPORT:
          render.setViewport(floats[i], floats[i + 1], floats[i + 2], floats[i + 3], floats[i + 4], floats[i + 5]);
          i += 6;
          break;
        case OP.SET_STENCIL_REFERENCE:
          render.setStencilReference(words[i++]);
          break;
        case OP.DRAW:
          (pass as GPURenderPassEncoder).draw(words[i], words[i + 1], words[i + 2], words[i + 3]);
          i += 4;
          break;
        case OP.DRAW_INDEXED:
          (pass as GPURenderPassEncoder).drawIndexed(words[i], words[i + 1], words[i + 2], words[i + 3] | 0, words[i + 4]);
          i += 5;
          break;
        case OP.DRAW_INDIRECT:
          (pass as GPURenderPassEncoder).drawIndirect(get(words[i]), u64(i + 1));
          i += 3;
          break;
        case OP.DRAW_INDEXED_INDIRECT:
          (pass as GPURenderPassEncoder).drawIndexedIndirect(get(words[i]), u64(i + 1));
          i += 3;
          break;
        case OP.EXECUTE_BUNDLES: {
          const n = words[i];
          render.executeBundles([...words.subarray(i + 1, i + 1 + n)].map(get));
          i += 1 + n;
          break;
        }
        case OP.BEGIN_OCCLUSION_QUERY:
          render.beginOcclusionQuery(words[i++]);
          break;
        case OP.END_OCCLUSION_QUERY:
          render.endOcclusionQuery();
          break;
        case OP.DISPATCH:
          (pass as GPUComputePassEncoder).dispatchWorkgroups(words[i], words[i + 1], words[i + 2]);
          i += 3;
          break;
        case OP.DISPATCH_INDIRECT:
          (pass as GPUComputePassEncoder).dispatchWorkgroupsIndirect(get(words[i]), u64(i + 1));
          i += 3;
          break;
        default:
          throw new Error(`rc_gpu: bad pass command ${op} at ${i - 1}`);
      }
    }
  };

  /** WebGPU has no clearTexture: a render pass that clears each layer and level. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const clearTexture = (encoder: GPUCommandEncoder, op: any) => {
    const texture = get(op.texture) as GPUTexture;
    if (!(texture.usage & RENDER_ATTACHMENT)) {
      console.warn(`rc_gpu: clearTexture of ${texture.label || 'a texture'} that is no render target: skipped`);
      return;
    }
    const depth = texture.format.startsWith('depth') || texture.format === 'stencil8';
    const levels = op.mipLevelCount ?? texture.mipLevelCount - op.baseMipLevel;
    const layers = op.arrayLayerCount ?? texture.depthOrArrayLayers - op.baseArrayLayer;
    for (let level = op.baseMipLevel; level < op.baseMipLevel + levels; level++) {
      for (let layer = op.baseArrayLayer; layer < op.baseArrayLayer + layers; layer++) {
        const view = texture.createView({ dimension: '2d', baseMipLevel: level, mipLevelCount: 1, baseArrayLayer: layer, arrayLayerCount: 1 });
        const hasStencil = texture.format.includes('stencil');
        const hasDepth = texture.format.startsWith('depth');
        encoder
          .beginRenderPass(
            depth
              ? {
                  colorAttachments: [],
                  depthStencilAttachment: {
                    view,
                    ...(hasDepth ? { depthLoadOp: 'clear', depthClearValue: 0, depthStoreOp: 'store' } : {}),
                    ...(hasStencil ? { stencilLoadOp: 'clear', stencilClearValue: 0, stencilStoreOp: 'store' } : {}),
                  },
                }
              : { colorAttachments: [{ view, loadOp: 'clear', clearValue: [0, 0, 0, 0], storeOp: 'store' }] },
          )
          .end();
      }
    }
  };

  /** Copies a read-back surface's frame into a 2D canvas the page can show. */
  const frames = new Map<Surface, OffscreenCanvas>();
  const readBack = async (s: Surface) => {
    const { width, height, format } = s.texture!;
    const row = Math.ceil((width * 4) / 256) * 256;
    const buffer = s.device!.createBuffer({ size: row * height, usage: COPY_DST | MAP_READ });
    const encoder = s.device!.createCommandEncoder();
    encoder.copyTextureToBuffer({ texture: s.texture! }, { buffer, bytesPerRow: row }, [width, height]);
    s.device!.queue.submit([encoder.finish()]);
    await buffer.mapAsync(1);
    const bytes = new Uint8Array(buffer.getMappedRange());
    const pixels = new Uint8ClampedArray(width * height * 4);
    const bgra = format.startsWith('bgra');
    for (let y = 0; y < height; y++) {
      const from = bytes.subarray(y * row, y * row + width * 4);
      const at = y * width * 4;
      pixels.set(from, at);
      if (bgra) for (let x = at; x < at + width * 4; x += 4) [pixels[x], pixels[x + 2]] = [pixels[x + 2], pixels[x]];
      for (let x = at + 3; x < at + width * 4; x += 4) pixels[x] = 255;
    }
    buffer.destroy();
    let canvas = frames.get(s);
    if (!canvas || canvas.width !== width || canvas.height !== height) frames.set(s, (canvas = new OffscreenCanvas(width, height)));
    canvas.getContext('2d')!.putImageData(new ImageData(pixels, width, height), 0, 0);
    s.canvas = canvas;
  };

  const imports = {
    adapter_info(adapter: number, out: number, cap: number) {
      const a = get(adapter) as GPUAdapter;
      const limits: Record<string, number> = {};
      for (const key in a.limits) limits[key] = (a.limits as unknown as Record<string, number>)[key];
      const info = a.info as GPUAdapterInfo & { isFallbackAdapter?: boolean; subgroupMinSize?: number; subgroupMaxSize?: number };
      return writeString(
        JSON.stringify({
          description: info.description,
          vendor: info.vendor,
          architecture: info.architecture,
          fallback: info.isFallbackAdapter ?? false,
          subgroupMinSize: info.subgroupMinSize,
          subgroupMaxSize: info.subgroupMaxSize,
          features: [...a.features],
          limits,
        }),
        out,
        cap,
      );
    },
    last_error: (out: number, cap: number) => writeString(lastError, out, cap),
    device_queue: (device: number) => add((get(device) as GPUDevice).queue),
    create(parent: number, kind: number, pointer: number, length: number) {
      const desc = json(pointer, length);
      try {
        return add(create(parent, kind, desc));
      } catch (e) {
        console.error('rc_gpu: create', kind, desc, e);
        throw e;
      }
    },
    release(id: number) {
      objects.delete(id);
    },
    destroy(id: number) {
      const object = objects.get(id);
      if (object instanceof GPUBuffer) mappings.delete(object);
      object?.destroy?.();
    },
    bind_group_layout: (pipeline: number, index: number) => add((get(pipeline) as GPURenderPipeline).getBindGroupLayout(index)),
    write_buffer(queue: number, buffer: number, offset: number, pointer: number, length: number) {
      const q = get(queue) as GPUQueue;
      try {
        q.writeBuffer(get(buffer), offset, data(pointer, length));
      } catch (e) {
        if (copyShared || !(e instanceof TypeError)) throw e;
        copyShared = true;
        q.writeBuffer(get(buffer), offset, data(pointer, length));
      }
    },
    write_texture(queue: number, pointer: number, length: number, dataPointer: number, dataLength: number) {
      const op = json(pointer, length);
      const q = get(queue) as GPUQueue;
      const destination = { ...op.destination, texture: get(op.destination.texture) };
      try {
        q.writeTexture(destination, data(dataPointer, dataLength), op.layout, op.size);
      } catch (e) {
        if (copyShared || !(e instanceof TypeError)) throw e;
        copyShared = true;
        q.writeTexture(destination, data(dataPointer, dataLength), op.layout, op.size);
      }
    },
    submit(queue: number, pointer: number, count: number) {
      const ids = memory.u32.slice(pointer >>> 2, (pointer >>> 2) + count);
      (get(queue) as GPUQueue).submit([...ids].map(get));
    },
    encode(encoder: number, pointer: number, length: number) {
      const e = get(encoder) as GPUCommandEncoder;
      const op = json(pointer, length);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const texture = (t: any) => ({ ...t, texture: get(t.texture) });
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const buffer = (b: any) => ({ ...b, buffer: get(b.buffer) });
      switch (op.op) {
        case 'copyBufferToBuffer': {
          const source = get(op.source) as GPUBuffer;
          e.copyBufferToBuffer(source, op.sourceOffset, get(op.destination), op.destinationOffset, op.size ?? source.size - op.sourceOffset);
          break;
        }
        case 'copyBufferToTexture':
          e.copyBufferToTexture(buffer(op.source), texture(op.destination), op.size);
          break;
        case 'copyTextureToBuffer':
          e.copyTextureToBuffer(texture(op.source), buffer(op.destination), op.size);
          break;
        case 'copyTextureToTexture':
          e.copyTextureToTexture(texture(op.source), texture(op.destination), op.size);
          break;
        case 'clearBuffer':
          e.clearBuffer(get(op.buffer), op.offset, op.size);
          break;
        case 'clearTexture':
          clearTexture(e, op);
          break;
        case 'resolveQuerySet':
          e.resolveQuerySet(get(op.querySet), op.firstQuery, op.queryCount, get(op.destination), op.destinationOffset);
          break;
        default:
          throw new Error(`rc_gpu: no encoder op ${op.op}`);
      }
    },
    finish(encoder: number) {
      const buffer = (get(encoder) as GPUCommandEncoder).finish();
      objects.delete(encoder);
      return add(buffer);
    },
    render_pass(encoder: number, pointer: number, length: number, ops: number, count: number) {
      const desc = json(pointer, length);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      desc.colorAttachments = desc.colorAttachments.map((a: any) =>
        a ? { ...a, view: get(a.view), resolveTarget: a.resolveTarget ? get(a.resolveTarget) : undefined } : null,
      );
      if (desc.depthStencilAttachment) desc.depthStencilAttachment.view = get(desc.depthStencilAttachment.view);
      if (desc.occlusionQuerySet) desc.occlusionQuerySet = get(desc.occlusionQuerySet);
      if (desc.timestampWrites) desc.timestampWrites.querySet = get(desc.timestampWrites.querySet);
      const pass = (get(encoder) as GPUCommandEncoder).beginRenderPass(desc);
      replay(pass, ops, count);
      pass.end();
    },
    compute_pass(encoder: number, pointer: number, length: number, ops: number, count: number) {
      const pass = (get(encoder) as GPUCommandEncoder).beginComputePass(json(pointer, length));
      replay(pass, ops, count);
      pass.end();
    },
    render_bundle(device: number, pointer: number, length: number, ops: number, count: number) {
      const { bundleLabel, ...desc } = json(pointer, length);
      const bundle = (get(device) as GPUDevice).createRenderBundleEncoder(desc);
      replay(bundle, ops, count);
      return add(bundle.finish({ label: bundleLabel }));
    },
    map(buffer: number, mode: number, offset: number, size: number) {
      const b = get(buffer) as GPUBuffer;
      mappings.set(b, { offset, size });
      return track(b.mapAsync(mode, offset, size));
    },
    work_done: (queue: number) => track((get(queue) as GPUQueue).onSubmittedWorkDone()),
    ticket_state(ticket: number) {
      const entry = tickets.get(ticket);
      if (!entry) return 2;
      if (entry.state) tickets.delete(ticket);
      return entry.state;
    },
    mapped_read(buffer: number, offset: number, out: number, length: number) {
      const b = get(buffer) as GPUBuffer;
      const mapping = mapped(b);
      mapping.bytes ??= b.getMappedRange(mapping.offset, mapping.size);
      memory.u8.set(new Uint8Array(mapping.bytes, offset - mapping.offset, length), out >>> 0);
    },
    mapped_write(buffer: number, offset: number, pointer: number, length: number) {
      const b = get(buffer) as GPUBuffer;
      const mapping = mapped(b);
      mapping.bytes ??= b.getMappedRange(mapping.offset, mapping.size);
      new Uint8Array(mapping.bytes, offset - mapping.offset, length).set(memory.u8.subarray(pointer >>> 0, (pointer >>> 0) + length));
    },
    unmap(buffer: number) {
      const b = get(buffer) as GPUBuffer;
      mappings.delete(b);
      b.unmap();
    },
    push_error_scope(device: number, filter: number) {
      (get(device) as GPUDevice).pushErrorScope((['validation', 'out-of-memory', 'internal'] as const)[filter]);
    },
    take_error(device: number, out: number, cap: number) {
      const message = uncaptured.get(get(device))?.shift();
      return message === undefined ? -1 : writeString(message, out, cap);
    },
    surface_create(window: number) {
      const canvas = new OffscreenCanvas(1, 1);
      const context = canvas.getContext('webgpu') as GPUCanvasContext | null;
      if (!context) throw new Error('rc_gpu: no WebGPU canvas context');
      return add({ canvas, context, window } satisfies Surface);
    },
    surface_configure(surface: number, device: number, pointer: number, length: number) {
      const s = get(surface) as Surface;
      const { width, height, ...config } = json(pointer, length);
      s.canvas.width = Math.max(1, width);
      s.canvas.height = Math.max(1, height);
      if (host.readback) {
        s.device = get(device);
        s.texture?.destroy();
        s.texture = s.device!.createTexture({
          size: [s.canvas.width, s.canvas.height],
          format: config.format,
          usage: config.usage | RENDER_ATTACHMENT | COPY_SRC,
          viewFormats: config.viewFormats,
        });
        return;
      }
      s.context.configure({ ...config, device: get(device) });
    },
    surface_texture(surface: number) {
      const s = get(surface) as Surface;
      return add(s.texture ?? s.context.getCurrentTexture());
    },
    preferred_format: (out: number, cap: number) => writeString(navigator.gpu.getPreferredCanvasFormat(), out, cap),
  };

  const suspending = {
    request_adapter: new WebAssembly.Suspending(async (high: number) => {
      const adapter = await navigator.gpu?.requestAdapter({ powerPreference: high ? 'high-performance' : 'low-power' });
      return adapter ? add(adapter) : 0;
    }),
    request_device: new WebAssembly.Suspending(async (adapter: number, pointer: number, length: number) => {
      try {
        const device = await (get(adapter) as GPUAdapter).requestDevice(json(pointer, length));
        const errors: string[] = [];
        uncaptured.set(device, errors);
        device.addEventListener('uncapturederror', (event) => {
          const message = (event as GPUUncapturedErrorEvent).error.message;
          console.error('WebGPU:', message);
          if (errors.length < 64) errors.push(message);
        });
        void device.lost.then((info) => console.error(`WebGPU device lost (${info.reason}): ${info.message}`));
        return add(device);
      } catch (e) {
        lastError = e instanceof Error ? e.message : String(e);
        return 0;
      }
    }),
    ticket_wait: new WebAssembly.Suspending(async (ticket: number) => {
      const entry = tickets.get(ticket);
      if (!entry) return 2;
      await entry.promise;
      tickets.delete(ticket);
      return entry.state;
    }),
    pop_error_scope: new WebAssembly.Suspending(async (device: number, out: number, cap: number) => {
      const error = await (get(device) as GPUDevice).popErrorScope();
      return error ? writeString(error.message, out, cap) : -1;
    }),
    // Presenting suspends the thread for a turn of this worker's event loop:
    // the browser frees transferred frames there (see platform.ts).
    // Then waits for the display's next refresh: wgpu's Fifo. Without it a
    // program draws as fast as the page takes frames (250+ a second while
    // Horizon loads), and the page and the GPU process lag behind it.
    surface_present: new WebAssembly.Suspending(async (surface: number) => {
      const s = get(surface) as Surface;
      if (s.texture && s.device) await readBack(s);
      host.present(s.window, s.canvas);
      await nextFrame();
    }),
  };

  return { ...imports, ...suspending };
}
