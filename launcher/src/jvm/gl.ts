// OpenGL 3.3 core on WebGL2, for the VM's `rjava_gl` imports (see
// vm/crates/jvm/src/web/gl.rs). Runs in the thread that renders, on the
// current context of its windows' canvases. GL names its objects with
// integers, WebGL with objects: each context keeps tables from one to the
// other. Pointers are addresses in the VM's memory.
//
// Emulated: base-vertex draws (attribute pointers shifted per draw), mapped
// buffer ranges (a block of the VM's memory, uploaded on unmap), glDrawBuffer,
// glEnablei/glDisablei, texture level queries; ignored: polygon modes,
// texture buffers (not in WebGL2; reported).

import { rewriteForWebGl } from './glsl';
import type { Memory } from './memory';

const GL_TEXTURE_2D = 0x0de1;
const GL_UNSIGNED_BYTE = 0x1401;
const GL_BYTE = 0x1400;
const GL_SHORT = 0x1402;
const GL_UNSIGNED_SHORT = 0x1403;
const GL_INT = 0x1404;
const GL_UNSIGNED_INT = 0x1405;
const GL_FLOAT = 0x1406;
const GL_HALF_FLOAT = 0x140b;
const GL_UNSIGNED_INT_24_8 = 0x84fa;
const GL_FLOAT_32_UNSIGNED_INT_24_8_REV = 0x8dad;
const GL_UNSIGNED_INT_2_10_10_10_REV = 0x8368;
const GL_UNSIGNED_INT_10F_11F_11F_REV = 0x8c3b;
const GL_UNSIGNED_INT_5_9_9_9_REV = 0x8c3e;
const GL_UNSIGNED_SHORT_5_6_5 = 0x8363;
const GL_UNSIGNED_SHORT_4_4_4_4 = 0x8033;
const GL_UNSIGNED_SHORT_5_5_5_1 = 0x8034;
const GL_DEPTH_COMPONENT = 0x1902;
const GL_DEPTH_STENCIL = 0x84f9;
const GL_RGBA = 0x1908;
const GL_RGB = 0x1907;
const GL_RED = 0x1903;
const GL_RG = 0x8227;
const GL_DEPTH_COMPONENT16 = 0x81a5;
const GL_DEPTH_COMPONENT24 = 0x81a6;
const GL_DEPTH_COMPONENT32 = 0x81a7;
const GL_DEPTH_COMPONENT32F = 0x8cac;
const GL_DEPTH24_STENCIL8 = 0x88f0;
const GL_BACK = 0x0405;
const GL_FRONT = 0x0404;
const GL_NONE = 0;
const GL_COMPILE_STATUS = 0x8b81;
const GL_INFO_LOG_LENGTH = 0x8b84;
const GL_DELETE_STATUS = 0x8b80;
const GL_SHADER_TYPE = 0x8b4f;
const GL_ARRAY_BUFFER = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER = 0x8893;
const GL_MAP_READ_BIT = 0x0001;
const GL_MAP_INVALIDATE_RANGE_BIT = 0x0004;
const GL_MAP_INVALIDATE_BUFFER_BIT = 0x0008;
const GL_MAP_FLUSH_EXPLICIT_BIT = 0x0010;
const GL_TIMEOUT_EXPIRED = 0x911b;
const GL_CONDITION_SATISFIED = 0x911c;
const GL_CLAMP_TO_BORDER = 0x812d;
const GL_CLAMP_TO_EDGE = 0x812f;
const GL_TEXTURE_WRAP_S = 0x2802;
const GL_TEXTURE_WRAP_T = 0x2803;
const GL_TEXTURE_WRAP_R = 0x8072;
const GL_TEXTURE_WIDTH = 0x1000;
const GL_TEXTURE_HEIGHT = 0x1001;
const GL_TEXTURE_INTERNAL_FORMAT = 0x1003;
const GL_TEXTURE_LOD_BIAS = 0x8501;
const GL_TEXTURE_MAX_ANISOTROPY = 0x84fe;
const GL_FRAMEBUFFER = 0x8d40;
const GL_DRAW_FRAMEBUFFER = 0x8ca9;
const GL_COLOR_ATTACHMENT0 = 0x8ce0;

/** The capabilities WebGL2 has (glEnable/glDisable of others are ignored:
 *  e.g. seamless cube maps, which WebGL2 always does). */
const CAPABILITIES = new Set([0x0be2, 0x0b44, 0x0b71, 0x0bd0, 0x8037, 0x809e, 0x80a0, 0x0c11, 0x0b90, 0x8c89]);
const GL_PIXEL_PACK_BUFFER = 0x88eb;
const GL_UNIFORM_BUFFER = 0x8a11;

/** Ignored texture and sampler parameters (not in WebGL2). */
const IGNORED_TEXTURE_PARAMETERS = new Set([GL_TEXTURE_LOD_BIAS, 0x1004 /* border color */, 0x8e42, 0x8e43, 0x8e44, 0x8e45 /* swizzles */]);

interface Attribute {
  buffer: WebGLBuffer | null;
  size: number;
  type: number;
  normalized: boolean;
  stride: number;
  offset: number;
  integer: boolean;
  divisor: number;
  enabled: boolean;
}

interface Vao {
  object: WebGLVertexArrayObject | null;
  attributes: Attribute[];
}

interface TextureInfo {
  object: WebGLTexture;
  levels: { width: number; height: number; internal: number }[];
}

interface Mapping {
  target: number;
  buffer: WebGLBuffer;
  offset: number;
  length: number;
  pointer: number;
  access: number;
}

interface ProgramInfo {
  object: WebGLProgram;
  /** Uniform locations by name, and the locations handed out (index + 1). */
  locationsByName: Map<string, number>;
  locations: (WebGLUniformLocation | null)[];
  /** Shaders attached, and the locations of texel buffer samplers. */
  shaders: Set<WebGLShader>;
  texelLocations: Set<number>;
  /** Uniform blocks: data size and binding (queried after linking). */
  blocks?: { size: number; binding: number }[];
}

/** A table from GL names to WebGL objects. */
class Names<T> {
  private objects = new Map<number, T>();
  private next = 1;
  add(object: T): number {
    const name = this.next++;
    this.objects.set(name, object);
    return name;
  }
  get(name: number): T | null {
    return name === 0 ? null : (this.objects.get(name) ?? null);
  }
  delete(name: number): T | null {
    const object = this.objects.get(name) ?? null;
    this.objects.delete(name);
    return object;
  }
  nameOf(object: T | null): number {
    if (object === null) return 0;
    for (const [name, o] of this.objects) if (o === object) return name;
    return 0;
  }
}

/** One WebGL2 context and its GL names and emulation state. */
class Context {
  textures = new Names<TextureInfo>();
  buffers = new Names<WebGLBuffer>();
  shaders = new Names<WebGLShader>();
  programs = new Names<ProgramInfo>();
  vaos = new Names<Vao>();
  framebuffers = new Names<WebGLFramebuffer>();
  renderbuffers = new Names<WebGLRenderbuffer>();
  samplers = new Names<WebGLSampler>();
  syncs = new Names<WebGLSync>();
  /** Queries are emulated: a time (ns) or elapsed time per query. */
  queries = new Names<{ start: number; result: number }>();
  activeQueries = new Map<number, { start: number; result: number }>();
  /** The texture bound to each unit and target (for level queries). */
  bound = new Map<string, TextureInfo>();
  activeUnit = 0;
  arrayBuffer: WebGLBuffer | null = null;
  vao: Vao;
  defaultVao: Vao;
  program: ProgramInfo | null = null;
  drawFramebuffer = 0;
  mappings = new Map<number, Mapping>();
  baseVertex: { drawElementsInstancedBaseVertexBaseInstanceWEBGL: Function } | null;
  drawBuffersIndexed: { enableiOES(target: number, index: number): void; disableiOES(target: number, index: number): void } | null;
  anisotropy: boolean;
  strings = new Map<number, number>();
  /** Buffer bindings by target, and buffer sizes (for texel buffers). */
  boundBuffers = new Map<number, WebGLBuffer | null>();
  bufferSizes = new Map<WebGLBuffer, number>();
  /** Copies of the contents of buffers mapped for writing (kept up to date
   *  by every CPU-side write): a mapping that must preserve the contents
   *  starts from the copy instead of reading the buffer back, which waits
   *  for the GPU. */
  shadows = new Map<WebGLBuffer, Uint8Array>();
  /** Texel buffers: textures mirroring a buffer, stale when it changes. */
  texelBuffers = new Map<TextureInfo, { buffer: WebGLBuffer; internal: number }>();
  /** Texel buffer sampler names of each shader. */
  shaderTexelBuffers = new Map<WebGLShader, string[]>();
  /** Buffers WebGL types as element (index) buffers: first bound there. */
  elementBuffers = new Set<WebGLBuffer>();
  typedBuffers = new Set<WebGLBuffer>();
  /** Uniform buffer bindings by index (size -1: the whole buffer). */
  uniformBuffers = new Map<number, { buffer: WebGLBuffer; offset: number; size: number }>();
  staleTexelBuffers = new Set<TextureInfo>();
  /** glPixelStorei values set (restored around texel buffer uploads). */
  pixelStore = new Map<number, number>();

  constructor(readonly gl: WebGL2RenderingContext) {
    this.defaultVao = { object: null, attributes: [] };
    this.vao = this.defaultVao;
    this.baseVertex = gl.getExtension('WEBGL_draw_instanced_base_vertex_base_instance');
    this.drawBuffersIndexed = gl.getExtension('OES_draw_buffers_indexed');
    this.anisotropy = gl.getExtension('EXT_texture_filter_anisotropic') !== null;
    gl.getExtension('EXT_color_buffer_float');
    gl.getExtension('EXT_color_buffer_half_float');
  }

  attribute(index: number): Attribute {
    const attributes = this.vao.attributes;
    while (attributes.length <= index) {
      attributes.push({ buffer: null, size: 4, type: GL_FLOAT, normalized: false, stride: 0, offset: 0, integer: false, divisor: 0, enabled: false });
    }
    return attributes[index];
  }
}

const TYPE_SIZE: Record<number, number> = {
  [GL_BYTE]: 1,
  [GL_UNSIGNED_BYTE]: 1,
  [GL_SHORT]: 2,
  [GL_UNSIGNED_SHORT]: 2,
  [GL_INT]: 4,
  [GL_UNSIGNED_INT]: 4,
  [GL_FLOAT]: 4,
  [GL_HALF_FLOAT]: 2,
  [GL_UNSIGNED_INT_2_10_10_10_REV]: 4,
};

const GL_TEXTURE_BUFFER = 0x8c2a;
const GL_PIXEL_UNPACK_BUFFER = 0x88ec;

/** Texel buffer formats: internal format → [format, type, bytes per texel]. */
const TEXEL_FORMATS: Record<number, [number, number, number]> = {
  0x8229: [GL_RED, GL_UNSIGNED_BYTE, 1], // R8
  0x822e: [GL_RED, GL_FLOAT, 4], // R32F
  0x822d: [GL_RED, GL_HALF_FLOAT, 2], // R16F
  0x8230: [GL_RG, GL_FLOAT, 8], // RG32F
  0x8814: [GL_RGBA, GL_FLOAT, 16], // RGBA32F
  0x881a: [GL_RGBA, GL_HALF_FLOAT, 8], // RGBA16F
  0x8058: [GL_RGBA, GL_UNSIGNED_BYTE, 4], // RGBA8
  0x8231: [0x8d94, GL_BYTE, 1], // R8I
  0x8232: [0x8d94, GL_UNSIGNED_BYTE, 1], // R8UI
  0x8233: [0x8d94, GL_SHORT, 2], // R16I
  0x8234: [0x8d94, GL_UNSIGNED_SHORT, 2], // R16UI
  0x8235: [0x8d94, GL_INT, 4], // R32I
  0x8236: [0x8d94, GL_UNSIGNED_INT, 4], // R32UI
  0x823b: [0x8228, GL_INT, 8], // RG32I
  0x823c: [0x8228, GL_UNSIGNED_INT, 8], // RG32UI
  0x8d8e: [0x8d99, GL_BYTE, 4], // RGBA8I
  0x8d7c: [0x8d99, GL_UNSIGNED_BYTE, 4], // RGBA8UI
  0x8d88: [0x8d99, GL_SHORT, 8], // RGBA16I
  0x8d76: [0x8d99, GL_UNSIGNED_SHORT, 8], // RGBA16UI
  0x8d82: [0x8d99, GL_INT, 16], // RGBA32I
  0x8d70: [0x8d99, GL_UNSIGNED_INT, 16], // RGBA32UI
};
const TEXEL_BUFFER_WIDTH = 4096;
/** Texel buffers on unit u are texture unit TEXEL_UNITS + u (GL binds them
 *  apart from 2D textures; here they are 2D textures too). */
const TEXEL_UNITS = 16;

/** The sized internal format WebGL2 needs for a GL one (GL accepts unsized). */
function sizedFormat(internal: number, type: number): number {
  switch (internal) {
    case GL_DEPTH_COMPONENT:
      return type === GL_FLOAT ? GL_DEPTH_COMPONENT32F : type === GL_UNSIGNED_SHORT ? GL_DEPTH_COMPONENT16 : GL_DEPTH_COMPONENT24;
    case GL_DEPTH_COMPONENT32:
      return GL_DEPTH_COMPONENT32F;
    case GL_DEPTH_STENCIL:
      return GL_DEPTH24_STENCIL8;
    case GL_RGBA:
      return type === GL_UNSIGNED_BYTE ? GL_RGBA : type === GL_FLOAT ? 0x8814 /* RGBA32F */ : type === GL_HALF_FLOAT ? 0x881a /* RGBA16F */ : GL_RGBA;
    case GL_RGB:
      return type === GL_UNSIGNED_BYTE ? GL_RGB : 0x8815;
    case GL_RED:
      return type === GL_FLOAT ? 0x822e /* R32F */ : 0x8229 /* R8 */;
    case GL_RG:
      return type === GL_FLOAT ? 0x8230 /* RG32F */ : 0x822b /* RG8 */;
    default:
      return internal;
  }
}

/** The pixel type WebGL2 accepts for an internal format (GL converts). */
function pixelType(internal: number, type: number): number {
  if (internal === GL_DEPTH_COMPONENT32F && type !== GL_FLOAT) return GL_FLOAT;
  if ((internal === GL_DEPTH_COMPONENT24 || internal === GL_DEPTH_COMPONENT16) && type === GL_FLOAT) return GL_UNSIGNED_INT;
  if (internal === GL_DEPTH24_STENCIL8) return GL_UNSIGNED_INT_24_8;
  return type;
}

export class GlEmulation {
  private contexts = new Map<number, Context>();
  private context: Context | null = null;
  private warned = new Set<string>();

  constructor(
    private memory: Memory,
    private allocate: (size: number) => number,
    private release: (pointer: number) => void,
  ) {}

  /** Creates the context of a window's canvas. */
  createContext(id: number, canvas: OffscreenCanvas): boolean {
    const gl = canvas.getContext('webgl2', {
      alpha: false,
      antialias: false,
      depth: true,
      stencil: true,
      premultipliedAlpha: false,
      preserveDrawingBuffer: false,
      powerPreference: 'high-performance',
    });
    if (!gl) return false;
    this.contexts.set(id, new Context(gl));
    return true;
  }

  makeCurrent(id: number) {
    this.context = this.contexts.get(id) ?? null;
  }

  destroyContext(id: number) {
    if (this.context === this.contexts.get(id)) this.context = null;
    this.contexts.delete(id);
  }

  private warn(what: string) {
    if (this.warned.has(what)) return;
    this.warned.add(what);
    console.warn(`[gl] ${what}`);
  }

  private get c(): Context {
    if (!this.context) throw new Error('no current GL context');
    return this.context;
  }

  private string(text: string): number {
    const bytes = new TextEncoder().encode(`${text}\0`);
    const pointer = this.allocate(bytes.length);
    this.memory.u8.set(bytes, pointer);
    return pointer;
  }

  private readString(pointer: number, length = -1): string {
    const u8 = this.memory.u8;
    const start = pointer >>> 0;
    let end = start;
    if (length >= 0) end = start + length;
    else while (u8[end] !== 0) end++;
    return new TextDecoder().decode(u8.slice(start, end));
  }

  private genNames(n: number, pointer: number, make: () => number) {
    const i32 = this.memory.i32;
    for (let i = 0; i < n; i++) i32[(pointer >>> 2) + i] = make();
  }

  private forNames(n: number, pointer: number, f: (name: number) => void) {
    const i32 = this.memory.i32;
    for (let i = 0; i < n; i++) f(i32[(pointer >>> 2) + i]);
  }

  /** A view of the VM's memory for pixel data of `type`, and the element offset of `pointer`. */
  private pixels(type: number, pointer: number): [ArrayBufferView, number] {
    const p = pointer >>> 0;
    switch (type) {
      case GL_FLOAT:
        return [this.memory.f32, p >>> 2];
      case GL_UNSIGNED_INT:
      case GL_UNSIGNED_INT_24_8:
      case GL_UNSIGNED_INT_2_10_10_10_REV:
      case GL_UNSIGNED_INT_10F_11F_11F_REV:
      case GL_UNSIGNED_INT_5_9_9_9_REV:
      case GL_FLOAT_32_UNSIGNED_INT_24_8_REV:
        return [this.memory.u32, p >>> 2];
      case GL_UNSIGNED_SHORT:
      case GL_HALF_FLOAT:
      case GL_UNSIGNED_SHORT_5_6_5:
      case GL_UNSIGNED_SHORT_4_4_4_4:
      case GL_UNSIGNED_SHORT_5_5_5_1:
        return [this.memory.u16, p >>> 1];
      case GL_SHORT:
        return [this.memory.i16, p >>> 1];
      case GL_INT:
        return [this.memory.i32, p >>> 2];
      case GL_BYTE:
        return [this.memory.i8, p];
      default:
        return [this.memory.u8, p];
    }
  }

  /** Whether a pixel unpack buffer is bound (pixel pointers are then offsets in it). */
  private unpackBuffer() {
    return !!this.c.boundBuffers.get(GL_PIXEL_UNPACK_BUFFER);
  }

  private textureKey(target: number) {
    return `${this.c.activeUnit}:${target}`;
  }

  private wrap(param: number) {
    return param === GL_CLAMP_TO_BORDER ? GL_CLAMP_TO_EDGE : param;
  }

  private shiftAttributes(base: number) {
    const { gl } = this.c;
    for (const [index, a] of this.c.vao.attributes.entries()) {
      if (!a.enabled || a.divisor !== 0 || !a.buffer) continue;
      const stride = a.stride || a.size * (TYPE_SIZE[a.type] ?? 4);
      gl.bindBuffer(GL_ARRAY_BUFFER, a.buffer);
      if (a.integer) gl.vertexAttribIPointer(index, a.size, a.type, a.stride, a.offset + base * stride);
      else gl.vertexAttribPointer(index, a.size, a.type, a.normalized, a.stride, a.offset + base * stride);
    }
    gl.bindBuffer(GL_ARRAY_BUFFER, this.c.arrayBuffer);
  }

  private drawBaseVertex(mode: number, count: number, type: number, indices: number, instances: number, base: number) {
    this.prepareDraw();
    const { gl, baseVertex } = this.c;
    if (base === 0) {
      gl.drawElementsInstanced(mode, count, type, indices >>> 0, instances);
    } else if (baseVertex) {
      baseVertex.drawElementsInstancedBaseVertexBaseInstanceWEBGL(mode, count, type, indices >>> 0, instances, base, 0);
    } else {
      this.shiftAttributes(base);
      gl.drawElementsInstanced(mode, count, type, indices >>> 0, instances);
      this.shiftAttributes(0);
    }
  }

  /** The `rjava_gl` imports; with `debug`, each call is followed by
   *  glGetError and failing calls are logged (slow). */
  imports(debug = false): Record<string, (...args: never[]) => unknown> {
    const functions = this.functions();
    if (!debug) return functions;
    const logged = new Map<string, number>();
    return Object.fromEntries(
      Object.entries(functions).map(([name, f]) => [
        name,
        (...args: never[]) => {
          const result = (f as (...a: never[]) => unknown)(...args);
          const error = this.context?.gl.getError() ?? 0;
          if (error !== 0) {
            const count = (logged.get(name) ?? 0) + 1;
            logged.set(name, count);
            if (count <= 3) console.warn(`[gl] ${name}(${args.join(', ')}) → error 0x${error.toString(16)}`);
          }
          return result;
        },
      ]),
    );
  }

  private functions(): Record<string, (...args: never[]) => unknown> {
    const self = this;
    const g = () => self.c.gl;
    return {
      glBindTexture(target: number, texture: number) {
        const info = self.c.textures.get(texture);
        // Texel buffers are 2D textures here, on their own units (see glTexBuffer).
        if (target === GL_TEXTURE_BUFFER) {
          g().activeTexture(0x84c0 + TEXEL_UNITS + self.c.activeUnit);
          g().bindTexture(GL_TEXTURE_2D, info?.object ?? null);
          g().activeTexture(0x84c0 + self.c.activeUnit);
        } else {
          g().bindTexture(target, info?.object ?? null);
        }
        if (info) self.c.bound.set(self.textureKey(target), info);
        else self.c.bound.delete(self.textureKey(target));
      },
      glBlendFunc: (s: number, d: number) => g().blendFunc(s, d),
      glClear: (mask: number) => g().clear(mask),
      glClearColor: (r: number, gr: number, b: number, a: number) => g().clearColor(r, gr, b, a),
      glClearDepth: (depth: number) => g().clearDepth(depth),
      glClearStencil: (s: number) => g().clearStencil(s),
      glColorMask: (r: number, gr: number, b: number, a: number) => g().colorMask(!!r, !!gr, !!b, !!a),
      glCullFace: (mode: number) => g().cullFace(mode),
      glDepthFunc: (func: number) => g().depthFunc(func),
      glDepthMask: (flag: number) => g().depthMask(!!flag),
      glDisable(cap: number) {
        if (CAPABILITIES.has(cap)) g().disable(cap);
      },
      glDrawArrays(mode: number, first: number, count: number) {
        self.prepareDraw();
        g().drawArrays(mode, first, count);
      },
      glDrawBuffer(buffer: number) {
        const target = self.c.drawFramebuffer === 0 ? (buffer === GL_NONE ? GL_NONE : GL_BACK) : buffer === GL_BACK || buffer === GL_FRONT ? GL_COLOR_ATTACHMENT0 : buffer;
        g().drawBuffers([target]);
      },
      glEnable(cap: number) {
        if (CAPABILITIES.has(cap)) g().enable(cap);
      },
      glFinish: () => g().finish(),
      glFlush: () => g().flush(),
      glFrontFace: (mode: number) => g().frontFace(mode),
      // A real glGetError would stall on the GPU process: report no error.
      glGetError: () => 0,
      glIsEnabled: (cap: number) => (CAPABILITIES.has(cap) && g().isEnabled(cap) ? 1 : 0),
      glLineWidth: (width: number) => g().lineWidth(width),
      glLogicOp: () => self.warn('glLogicOp is not supported'),
      glPixelStorei(pname: number, param: number) {
        // Byte swapping and LSB-first are not in WebGL2.
        if (pname === 0x0cf0 || pname === 0x0cf1 || pname === 0x0d00 || pname === 0x0d01) return;
        self.c.pixelStore.set(pname, param);
        g().pixelStorei(pname, param);
      },
      glPolygonMode(_face: number, mode: number) {
        if (mode !== 0x1b02 /* FILL */) self.warn('glPolygonMode other than GL_FILL is not supported');
      },
      glPolygonOffset: (factor: number, units: number) => g().polygonOffset(factor, units),
      glReadBuffer: (src: number) => g().readBuffer(src),
      glScissor: (x: number, y: number, w: number, h: number) => g().scissor(x, y, w, h),
      glStencilFunc: (func: number, ref: number, mask: number) => g().stencilFunc(func, ref, mask),
      glStencilMask: (mask: number) => g().stencilMask(mask),
      glStencilOp: (fail: number, zfail: number, zpass: number) => g().stencilOp(fail, zfail, zpass),
      glTexParameterf(target: number, pname: number, param: number) {
        if (IGNORED_TEXTURE_PARAMETERS.has(pname)) return;
        if (pname === GL_TEXTURE_MAX_ANISOTROPY && !self.c.anisotropy) return;
        g().texParameterf(target, pname, param);
      },
      glTexParameteri(target: number, pname: number, param: number) {
        if (IGNORED_TEXTURE_PARAMETERS.has(pname)) return;
        if (pname === GL_TEXTURE_WRAP_S || pname === GL_TEXTURE_WRAP_T || pname === GL_TEXTURE_WRAP_R) param = self.wrap(param);
        g().texParameteri(target, pname, param);
      },
      glViewport: (x: number, y: number, w: number, h: number) => g().viewport(x, y, w, h),
      nglDeleteTextures(n: number, pointer: number) {
        self.forNames(n, pointer, (name) => {
          const info = self.c.textures.delete(name);
          if (info) g().deleteTexture(info.object);
        });
      },
      nglDrawElements(mode: number, count: number, type: number, indices: number) {
        self.prepareDraw();
        g().drawElements(mode, count, type, indices >>> 0);
      },
      nglGenTextures(n: number, pointer: number) {
        self.genNames(n, pointer, () => self.c.textures.add({ object: g().createTexture()!, levels: [] }));
      },
      nglGetFloatv(pname: number, params: number) {
        const value = g().getParameter(pname);
        const f32 = self.memory.f32;
        if (typeof value === 'number') f32[params >>> 2] = value;
        else if (value instanceof Float32Array || value instanceof Int32Array) f32.set(value, params >>> 2);
      },
      nglGetIntegerv(pname: number, params: number) {
        const i32 = self.memory.i32;
        const at = params >>> 2;
        switch (pname) {
          case 0x821b: // MAJOR_VERSION
          case 0x821c: // MINOR_VERSION
            i32[at] = 3;
            return;
          case 0x821d: // NUM_EXTENSIONS
          case 0x821e: // CONTEXT_FLAGS
            i32[at] = 0;
            return;
          case 0x9126: // CONTEXT_PROFILE_MASK
            i32[at] = 1;
            return;
          case 0x8b4a: // MAX_VERTEX_UNIFORM_COMPONENTS
          case 0x8b49: // MAX_FRAGMENT_UNIFORM_COMPONENTS
            i32[at] = g().getParameter(pname);
            return;
        }
        const value = g().getParameter(pname);
        if (typeof value === 'number') i32[at] = value;
        else if (typeof value === 'boolean') i32[at] = value ? 1 : 0;
        else if (value instanceof Int32Array || value instanceof Float32Array || value instanceof Uint32Array) {
          for (let i = 0; i < value.length; i++) i32[at + i] = value[i];
        } else if (value === null) i32[at] = 0;
        else if (value instanceof WebGLTexture) i32[at] = 0;
        else i32[at] = 0;
      },
      nglGetString(name: number) {
        let pointer = self.c.strings.get(name);
        if (pointer === undefined) {
          const gl = g();
          const debug = gl.getExtension('WEBGL_debug_renderer_info');
          const text =
            name === 0x1f00 // VENDOR
              ? String(debug ? gl.getParameter(debug.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR))
              : name === 0x1f01 // RENDERER
                ? String(debug ? gl.getParameter(debug.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER))
                : name === 0x1f02 // VERSION
                  ? '3.3.0 WebGL 2.0'
                  : name === 0x8b8c // SHADING_LANGUAGE_VERSION
                    ? '3.30'
                    : '';
          pointer = self.string(text);
          self.c.strings.set(name, pointer);
        }
        return pointer;
      },
      nglGetStringi: () => 0,
      nglGetTexImage: () => self.warn('glGetTexImage is not supported'),
      nglGetTexLevelParameteriv(target: number, level: number, pname: number, params: number) {
        const info = self.c.bound.get(self.textureKey(target));
        const l = info?.levels[level];
        const value = !l ? 0 : pname === GL_TEXTURE_WIDTH ? l.width : pname === GL_TEXTURE_HEIGHT ? l.height : pname === GL_TEXTURE_INTERNAL_FORMAT ? l.internal : 0;
        self.memory.i32[params >>> 2] = value;
      },
      nglReadPixels(x: number, y: number, w: number, h: number, format: number, type: number, pixels: number) {
        const pack = self.c.boundBuffers.get(GL_PIXEL_PACK_BUFFER);
        if (pack) {
          self.c.shadows.delete(pack);
          return g().readPixels(x, y, w, h, format, type, pixels >>> 0);
        }
        const [view, offset] = self.pixels(type, pixels);
        g().readPixels(x, y, w, h, format, type, view, offset);
      },
      nglTexImage2D(target: number, level: number, internal: number, w: number, h: number, border: number, format: number, type: number, pixels: number) {
        // Proxy textures (size probes) do not exist in WebGL: the probe fails,
        // and the game falls back to GL_MAX_TEXTURE_SIZE.
        if (target === 0x8064) return;
        const sized = sizedFormat(internal, type);
        const t = pixelType(sized, type);
        const info = self.c.bound.get(self.textureKey(target));
        if (info) info.levels[level] = { width: w, height: h, internal };
        if (self.unpackBuffer()) {
          g().texImage2D(target, level, sized, w, h, border, format, t, pixels >>> 0);
        } else if (pixels === 0) {
          g().texImage2D(target, level, sized, w, h, border, format, t, null);
        } else {
          const [view, offset] = self.pixels(t, pixels);
          g().texImage2D(target, level, sized, w, h, border, format, t, view, offset);
        }
      },
      nglTexSubImage2D(target: number, level: number, x: number, y: number, w: number, h: number, format: number, type: number, pixels: number) {
        if (self.unpackBuffer()) return g().texSubImage2D(target, level, x, y, w, h, format, type, pixels >>> 0);
        const [view, offset] = self.pixels(type, pixels);
        g().texSubImage2D(target, level, x, y, w, h, format, type, view, offset);
      },
      nglTexImage3D(target: number, level: number, internal: number, w: number, h: number, d: number, border: number, format: number, type: number, pixels: number) {
        const sized = sizedFormat(internal, type);
        const t = pixelType(sized, type);
        if (self.unpackBuffer()) g().texImage3D(target, level, sized, w, h, d, border, format, t, pixels >>> 0);
        else if (pixels === 0) g().texImage3D(target, level, sized, w, h, d, border, format, t, null);
        else {
          const [view, offset] = self.pixels(t, pixels);
          g().texImage3D(target, level, sized, w, h, d, border, format, t, view, offset);
        }
      },
      nglTexSubImage3D(target: number, level: number, x: number, y: number, z: number, w: number, h: number, d: number, format: number, type: number, pixels: number) {
        if (self.unpackBuffer()) return g().texSubImage3D(target, level, x, y, z, w, h, d, format, type, pixels >>> 0);
        const [view, offset] = self.pixels(type, pixels);
        g().texSubImage3D(target, level, x, y, z, w, h, d, format, type, view, offset);
      },
      glActiveTexture(texture: number) {
        self.c.activeUnit = texture - 0x84c0;
        g().activeTexture(texture);
      },
      glBlendEquation: (mode: number) => g().blendEquation(mode),
      glBlendFuncSeparate: (a: number, b: number, c: number, d: number) => g().blendFuncSeparate(a, b, c, d),
      glBindBuffer(target: number, buffer: number) {
        const object = self.c.buffers.get(buffer);
        self.c.boundBuffers.set(target, object);
        // WebGL types a buffer by its first binding (index buffers apart).
        if (object && !self.c.typedBuffers.has(object)) {
          self.c.typedBuffers.add(object);
          if (target === GL_ELEMENT_ARRAY_BUFFER) self.c.elementBuffers.add(object);
        }
        if (target === GL_ARRAY_BUFFER) self.c.arrayBuffer = object;
        g().bindBuffer(target, object);
      },
      glUnmapBuffer(target: number) {
        const mapping = self.c.mappings.get(target);
        if (!mapping) return 0;
        self.c.mappings.delete(target);
        if (!(mapping.access & GL_MAP_FLUSH_EXPLICIT_BIT) && mapping.access & 0x0002 /* WRITE */) {
          g().bufferSubData(target, mapping.offset, self.memory.u8, mapping.pointer, mapping.length);
          self.shadowWrite(target, mapping.offset, mapping.pointer, mapping.length);
        }
        self.release(mapping.pointer);
        self.changed(target);
        return 1;
      },
      nglBufferData(target: number, size: number, data: number, usage: number) {
        // Sizes rounded up to 16 bytes: a buffer may back a uniform block,
        // whose std140 size is a multiple of 16 (a 12-byte vec3 block needs 16).
        const padded = Math.ceil(size / 16) * 16;
        if (data === 0 || padded !== size) g().bufferData(target, padded, usage);
        if (data !== 0) {
          if (padded === size) g().bufferData(target, self.memory.u8, usage, data >>> 0, size);
          else g().bufferSubData(target, 0, self.memory.u8, data >>> 0, size);
        }
        const buffer = self.c.boundBuffers.get(target);
        if (buffer) {
          self.c.bufferSizes.set(buffer, padded);
          if (self.c.shadows.has(buffer)) {
            const shadow = new Uint8Array(padded);
            if (data !== 0) shadow.set(self.memory.u8.subarray(data >>> 0, (data >>> 0) + size));
            self.c.shadows.set(buffer, shadow);
          }
        }
        self.changed(target);
      },
      nglBufferSubData(target: number, offset: number, size: number, data: number) {
        g().bufferSubData(target, offset, self.memory.u8, data >>> 0, size);
        self.shadowWrite(target, offset, data >>> 0, size);
        self.changed(target);
      },
      nglDeleteBuffers(n: number, pointer: number) {
        self.forNames(n, pointer, (name) => {
          const object = self.c.buffers.delete(name);
          if (object) {
            g().deleteBuffer(object);
            self.c.shadows.delete(object);
          }
        });
      },
      nglGenBuffers(n: number, pointer: number) {
        self.genNames(n, pointer, () => self.c.buffers.add(g().createBuffer()!));
      },
      nglGetBufferSubData: (target: number, offset: number, size: number, data: number) =>
        g().getBufferSubData(target, offset, self.memory.u8, data >>> 0, size),
      glAttachShader(program: number, shader: number) {
        const info = self.c.programs.get(program)!;
        const object = self.c.shaders.get(shader)!;
        info.shaders.add(object);
        g().attachShader(info.object, object);
      },
      glBlendEquationSeparate: (rgb: number, alpha: number) => g().blendEquationSeparate(rgb, alpha),
      glCompileShader: (shader: number) => g().compileShader(self.c.shaders.get(shader)!),
      glCreateProgram: () =>
        self.c.programs.add({ object: g().createProgram()!, locationsByName: new Map(), locations: [], shaders: new Set(), texelLocations: new Set() }),
      glCreateShader: (type: number) => self.c.shaders.add(g().createShader(type)!),
      glDeleteProgram(program: number) {
        const info = self.c.programs.delete(program);
        if (info) g().deleteProgram(info.object);
      },
      glDeleteShader(shader: number) {
        const object = self.c.shaders.delete(shader);
        if (object) g().deleteShader(object);
      },
      glDetachShader(program: number, shader: number) {
        const info = self.c.programs.get(program)!;
        const object = self.c.shaders.get(shader)!;
        info.shaders.delete(object);
        g().detachShader(info.object, object);
      },
      glDisableVertexAttribArray(index: number) {
        self.c.attribute(index).enabled = false;
        g().disableVertexAttribArray(index);
      },
      glEnableVertexAttribArray(index: number) {
        self.c.attribute(index).enabled = true;
        g().enableVertexAttribArray(index);
      },
      glLinkProgram(program: number) {
        const info = self.c.programs.get(program)!;
        info.locationsByName.clear();
        info.locations = [];
        info.blocks = undefined;
        info.texelLocations.clear();
        g().linkProgram(info.object);
      },
      glUniform1f(location: number, v: number) {
        g().uniform1f(self.c.program?.locations[location - 1] ?? null, v);
      },
      glUniform1i(location: number, v: number) {
        if (self.c.program?.texelLocations.has(location)) v += TEXEL_UNITS;
        g().uniform1i(self.c.program?.locations[location - 1] ?? null, v);
      },
      glUseProgram(program: number) {
        self.c.program = self.c.programs.get(program);
        g().useProgram(self.c.program?.object ?? null);
      },
      nglBindAttribLocation: (program: number, index: number, name: number) =>
        g().bindAttribLocation(self.c.programs.get(program)!.object, index, self.readString(name)),
      nglDrawBuffers(n: number, bufs: number) {
        const list: number[] = [];
        self.forNames(n, bufs, (b) => list.push(b));
        g().drawBuffers(list);
      },
      nglGetAttribLocation: (program: number, name: number) => g().getAttribLocation(self.c.programs.get(program)!.object, self.readString(name)),
      nglGetProgramInfoLog(program: number, size: number, length: number, log: number) {
        self.writeLog(g().getProgramInfoLog(self.c.programs.get(program)!.object) ?? '', size, length, log);
      },
      nglGetProgramiv(program: number, pname: number, params: number) {
        const info = self.c.programs.get(program)!;
        const value =
          pname === GL_INFO_LOG_LENGTH
            ? new TextEncoder().encode(g().getProgramInfoLog(info.object) ?? '').length + 1
            : g().getProgramParameter(info.object, pname);
        self.memory.i32[params >>> 2] = typeof value === 'boolean' ? (value ? 1 : 0) : Number(value ?? 0);
      },
      nglGetShaderInfoLog(shader: number, size: number, length: number, log: number) {
        self.writeLog(g().getShaderInfoLog(self.c.shaders.get(shader)!) ?? '', size, length, log);
      },
      nglGetShaderiv(shader: number, pname: number, params: number) {
        const object = self.c.shaders.get(shader)!;
        let value: unknown;
        if (pname === GL_INFO_LOG_LENGTH) value = new TextEncoder().encode(g().getShaderInfoLog(object) ?? '').length + 1;
        else if (pname === GL_COMPILE_STATUS || pname === GL_DELETE_STATUS || pname === GL_SHADER_TYPE) value = g().getShaderParameter(object, pname);
        else value = 0;
        if (pname === GL_COMPILE_STATUS && !value) console.warn(`[gl] shader compilation failed:\n${g().getShaderInfoLog(object)}\n${g().getShaderSource(object)}`);
        self.memory.i32[params >>> 2] = typeof value === 'boolean' ? (value ? 1 : 0) : Number(value);
      },
      nglGetUniformLocation(program: number, name: number) {
        const info = self.c.programs.get(program)!;
        const text = self.readString(name);
        let location = info.locationsByName.get(text);
        if (location === undefined) {
          const object = g().getUniformLocation(info.object, text);
          location = object === null ? -1 : info.locations.push(object);
          info.locationsByName.set(text, location);
          if ([...info.shaders].some((s) => self.c.shaderTexelBuffers.get(s)?.includes(text))) info.texelLocations.add(location);
        }
        return location;
      },
      nglShaderSource(shader: number, count: number, strings: number, lengths: number) {
        const u32 = self.memory.u32;
        const i32 = self.memory.i32;
        let source = '';
        for (let i = 0; i < count; i++) {
          const length = lengths === 0 ? -1 : i32[(lengths >>> 2) + i];
          source += self.readString(u32[(strings >>> 2) + i], length);
        }
        const object = self.c.shaders.get(shader)!;
        const rewritten = rewriteForWebGl(source, g().getShaderParameter(object, GL_SHADER_TYPE) === 0x8b30);
        self.c.shaderTexelBuffers.set(object, rewritten.texelBuffers);
        g().shaderSource(object, rewritten.source);
      },
      nglUniform4fv: (location: number, count: number, value: number) =>
        g().uniform4fv(self.c.program?.locations[location - 1] ?? null, self.memory.f32, value >>> 2, count * 4),
      nglUniformMatrix4fv: (location: number, count: number, transpose: number, value: number) =>
        g().uniformMatrix4fv(self.c.program?.locations[location - 1] ?? null, !!transpose, self.memory.f32, value >>> 2, count * 16),
      nglVertexAttribPointer(index: number, size: number, type: number, normalized: number, stride: number, pointer: number) {
        Object.assign(self.c.attribute(index), { buffer: self.c.arrayBuffer, size, type, normalized: !!normalized, stride, offset: pointer >>> 0, integer: false });
        g().vertexAttribPointer(index, size, type, !!normalized, stride, pointer >>> 0);
      },
      glBindBufferBase(target: number, index: number, buffer: number) {
        const object = self.c.buffers.get(buffer);
        g().bindBufferBase(target, index, object);
        // (Also binds the target's general binding point.)
        self.c.boundBuffers.set(target, object);
        if (target === GL_UNIFORM_BUFFER) self.trackUniformBuffer(index, object, 0, -1);
      },
      glBindBufferRange(target: number, index: number, buffer: number, offset: number, size: number) {
        const object = self.c.buffers.get(buffer);
        g().bindBufferRange(target, index, object, offset, size);
        self.c.boundBuffers.set(target, object);
        if (target === GL_UNIFORM_BUFFER) self.trackUniformBuffer(index, object, offset, size);
      },
      glBindFramebuffer(target: number, framebuffer: number) {
        if (target === GL_FRAMEBUFFER || target === GL_DRAW_FRAMEBUFFER) self.c.drawFramebuffer = framebuffer;
        g().bindFramebuffer(target, self.c.framebuffers.get(framebuffer));
      },
      glBindRenderbuffer: (target: number, renderbuffer: number) => g().bindRenderbuffer(target, self.c.renderbuffers.get(renderbuffer)),
      glBindVertexArray(array: number) {
        const vao = self.c.vaos.get(array) ?? self.c.defaultVao;
        self.c.vao = vao;
        g().bindVertexArray(vao.object);
      },
      glBlitFramebuffer: (a: number, b: number, c: number, d: number, e: number, f: number, h: number, i: number, mask: number, filter: number) =>
        g().blitFramebuffer(a, b, c, d, e, f, h, i, mask, filter),
      glCheckFramebufferStatus: (target: number) => g().checkFramebufferStatus(target),
      glDisablei(target: number, index: number) {
        if (self.c.drawBuffersIndexed) self.c.drawBuffersIndexed.disableiOES(target, index);
        else if (index === 0) g().disable(target);
      },
      glEnablei(target: number, index: number) {
        if (self.c.drawBuffersIndexed) self.c.drawBuffersIndexed.enableiOES(target, index);
        else if (index === 0) g().enable(target);
      },
      glFlushMappedBufferRange(target: number, offset: number, length: number) {
        const mapping = self.c.mappings.get(target);
        if (!mapping) return;
        g().bufferSubData(target, mapping.offset + offset, self.memory.u8, mapping.pointer + offset, length);
        self.shadowWrite(target, mapping.offset + offset, mapping.pointer + offset, length);
      },
      glFramebufferRenderbuffer: (target: number, attachment: number, rbtarget: number, renderbuffer: number) =>
        g().framebufferRenderbuffer(target, attachment, rbtarget, self.c.renderbuffers.get(renderbuffer)),
      glFramebufferTexture2D: (target: number, attachment: number, textarget: number, texture: number, level: number) =>
        g().framebufferTexture2D(target, attachment, textarget, self.c.textures.get(texture)?.object ?? null, level),
      glFramebufferTextureLayer: (target: number, attachment: number, texture: number, level: number, layer: number) =>
        g().framebufferTextureLayer(target, attachment, self.c.textures.get(texture)?.object ?? null, level, layer),
      glGenerateMipmap: (target: number) => g().generateMipmap(target),
      glRenderbufferStorage: (target: number, internal: number, w: number, h: number) => g().renderbufferStorage(target, sizedFormat(internal, 0), w, h),
      glVertexAttribI4i: (index: number, x: number, y: number, z: number, w: number) => g().vertexAttribI4i(index, x, y, z, w),
      nglClearBufferfv: (buffer: number, drawbuffer: number, value: number) => g().clearBufferfv(buffer, drawbuffer, self.memory.f32, value >>> 2),
      nglClearBufferiv: (buffer: number, drawbuffer: number, value: number) => g().clearBufferiv(buffer, drawbuffer, self.memory.i32, value >>> 2),
      nglDeleteFramebuffers(n: number, pointer: number) {
        self.forNames(n, pointer, (name) => {
          const object = self.c.framebuffers.delete(name);
          if (object) g().deleteFramebuffer(object);
        });
      },
      nglDeleteRenderbuffers(n: number, pointer: number) {
        self.forNames(n, pointer, (name) => {
          const object = self.c.renderbuffers.delete(name);
          if (object) g().deleteRenderbuffer(object);
        });
      },
      nglDeleteVertexArrays(n: number, pointer: number) {
        self.forNames(n, pointer, (name) => {
          const vao = self.c.vaos.delete(name);
          if (vao?.object) g().deleteVertexArray(vao.object);
        });
      },
      nglGenFramebuffers(n: number, pointer: number) {
        self.genNames(n, pointer, () => self.c.framebuffers.add(g().createFramebuffer()!));
      },
      nglGenRenderbuffers(n: number, pointer: number) {
        self.genNames(n, pointer, () => self.c.renderbuffers.add(g().createRenderbuffer()!));
      },
      nglGenVertexArrays(n: number, pointer: number) {
        self.genNames(n, pointer, () => self.c.vaos.add({ object: g().createVertexArray()!, attributes: [] }));
      },
      nglMapBufferRange(target: number, offset: number, length: number, access: number) {
        const buffer = g().getParameter(target === GL_ELEMENT_ARRAY_BUFFER ? 0x8895 : target === GL_ARRAY_BUFFER ? 0x8894 : bindingQuery(target)) as WebGLBuffer;
        const pointer = self.allocate(Math.max(length, 1));
        if (access & GL_MAP_READ_BIT) {
          g().getBufferSubData(target, offset, self.memory.u8, pointer, length);
        } else if (!(access & (GL_MAP_INVALIDATE_RANGE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT))) {
          // The contents must be preserved: from the buffer's copy (made by
          // reading the buffer back once).
          let shadow = self.c.shadows.get(buffer);
          const size = self.c.bufferSizes.get(buffer);
          if (!shadow && size !== undefined) {
            shadow = new Uint8Array(size);
            g().getBufferSubData(target, 0, shadow);
            self.c.shadows.set(buffer, shadow);
          }
          if (shadow) self.memory.u8.set(shadow.subarray(offset, offset + length), pointer);
          else g().getBufferSubData(target, offset, self.memory.u8, pointer, length);
        }
        self.c.mappings.set(target, { target, buffer, offset, length, pointer, access });
        return pointer;
      },
      nglVertexAttribIPointer(index: number, size: number, type: number, stride: number, pointer: number) {
        Object.assign(self.c.attribute(index), { buffer: self.c.arrayBuffer, size, type, normalized: false, stride, offset: pointer >>> 0, integer: true });
        g().vertexAttribIPointer(index, size, type, stride, pointer >>> 0);
      },
      glCopyBufferSubData(read: number, write: number, roffset: number, woffset: number, size: number) {
        const source = self.c.boundBuffers.get(read);
        const target = self.c.boundBuffers.get(write);
        if (target && source && self.c.elementBuffers.has(target) !== self.c.elementBuffers.has(source)) {
          // WebGL copies index buffers only to and from index buffers: through the CPU.
          self.warn('glCopyBufferSubData between index and other buffers goes through the CPU (slow)');
          const bytes = new Uint8Array(size);
          g().getBufferSubData(read, roffset, bytes);
          g().bufferSubData(write, woffset, bytes);
        } else {
          g().copyBufferSubData(read, write, roffset, woffset, size);
        }
        const shadow = target && self.c.shadows.get(target);
        if (shadow) {
          const from = source && self.c.shadows.get(source);
          if (from) shadow.set(from.subarray(roffset, roffset + size), woffset);
          else self.c.shadows.delete(target!);
        }
        self.changed(write);
      },
      glDrawArraysInstanced(mode: number, first: number, count: number, instances: number) {
        self.prepareDraw();
        g().drawArraysInstanced(mode, first, count, instances);
      },
      glTexBuffer(_target: number, internal: number, buffer: number) {
        const info = self.c.bound.get(self.textureKey(GL_TEXTURE_BUFFER));
        const object = self.c.buffers.get(buffer);
        if (!info) return;
        if (!object) {
          self.c.texelBuffers.delete(info);
          return;
        }
        self.c.texelBuffers.set(info, { buffer: object, internal });
        self.uploadTexelBuffer(info);
      },
      glUniformBlockBinding(program: number, index: number, binding: number) {
        const info = self.c.programs.get(program)!;
        g().uniformBlockBinding(info.object, index, binding);
        if (info.blocks?.[index]) info.blocks[index].binding = binding;
      },
      nglDrawElementsInstanced(mode: number, count: number, type: number, indices: number, instances: number) {
        self.prepareDraw();
        g().drawElementsInstanced(mode, count, type, indices >>> 0, instances);
      },
      nglGetUniformBlockIndex: (program: number, name: number) => g().getUniformBlockIndex(self.c.programs.get(program)!.object, self.readString(name)),
      glDeleteSync(sync: number) {
        const object = self.c.syncs.delete(sync);
        if (object) g().deleteSync(object);
      },
      glFenceSync: (condition: number, flags: number) => self.c.syncs.add(g().fenceSync(condition, flags)!),
      nglClientWaitSync(sync: number, flags: number, timeout: bigint) {
        const object = self.c.syncs.get(sync);
        if (!object) return GL_CONDITION_SATISFIED;
        // WebGL allows no client wait timeout: wait by finishing the GPU's work.
        const status = g().clientWaitSync(object, flags, 0);
        if (status === GL_TIMEOUT_EXPIRED && timeout !== 0n) {
          g().finish();
          return GL_CONDITION_SATISFIED;
        }
        return status;
      },
      nglDrawElementsBaseVertex: (mode: number, count: number, type: number, indices: number, base: number) =>
        self.drawBaseVertex(mode, count, type, indices, 1, base),
      nglDrawElementsInstancedBaseVertex: (mode: number, count: number, type: number, indices: number, instances: number, base: number) =>
        self.drawBaseVertex(mode, count, type, indices, instances, base),
      glBindSampler: (unit: number, sampler: number) => g().bindSampler(unit, self.c.samplers.get(sampler)),
      // Queries (timestamps, elapsed time; other kinds report 1) on the CPU clock.
      nglGenQueries(n: number, pointer: number) {
        self.genNames(n, pointer, () => self.c.queries.add({ start: 0, result: 0 }));
      },
      nglDeleteQueries(n: number, pointer: number) {
        self.forNames(n, pointer, (name) => self.c.queries.delete(name));
      },
      glBeginQuery(target: number, id: number) {
        const query = self.c.queries.get(id);
        if (!query) return;
        query.start = nowNs();
        self.c.activeQueries.set(target, query);
      },
      glEndQuery(target: number) {
        const query = self.c.activeQueries.get(target);
        if (!query) return;
        query.result = target === 0x88bf /* TIME_ELAPSED */ ? nowNs() - query.start : 1;
        self.c.activeQueries.delete(target);
      },
      glQueryCounter(id: number) {
        const query = self.c.queries.get(id);
        if (query) query.result = nowNs();
      },
      nglGetQueryiv(_target: number, _pname: number, params: number) {
        self.memory.i32[params >>> 2] = 64; // QUERY_COUNTER_BITS
      },
      nglGetQueryObjectiv: (id: number, pname: number, params: number) => self.queryResult(id, pname, params, false),
      nglGetQueryObjectuiv: (id: number, pname: number, params: number) => self.queryResult(id, pname, params, false),
      nglGetQueryObjecti64v: (id: number, pname: number, params: number) => self.queryResult(id, pname, params, true),
      nglGetQueryObjectui64v: (id: number, pname: number, params: number) => self.queryResult(id, pname, params, true),
      glSamplerParameterf(sampler: number, pname: number, param: number) {
        if (IGNORED_TEXTURE_PARAMETERS.has(pname)) return;
        if (pname === GL_TEXTURE_MAX_ANISOTROPY && !self.c.anisotropy) return;
        g().samplerParameterf(self.c.samplers.get(sampler)!, pname, param);
      },
      glSamplerParameteri(sampler: number, pname: number, param: number) {
        if (IGNORED_TEXTURE_PARAMETERS.has(pname)) return;
        if (pname === GL_TEXTURE_WRAP_S || pname === GL_TEXTURE_WRAP_T || pname === GL_TEXTURE_WRAP_R) param = self.wrap(param);
        g().samplerParameteri(self.c.samplers.get(sampler)!, pname, param);
      },
      glVertexAttribDivisor(index: number, divisor: number) {
        self.c.attribute(index).divisor = divisor;
        g().vertexAttribDivisor(index, divisor);
      },
      nglDeleteSamplers(n: number, pointer: number) {
        self.forNames(n, pointer, (name) => {
          const object = self.c.samplers.delete(name);
          if (object) g().deleteSampler(object);
        });
      },
      nglGenSamplers(n: number, pointer: number) {
        self.genNames(n, pointer, () => self.c.samplers.add(g().createSampler()!));
      },
    };
  }

  private queryResult(id: number, pname: number, params: number, wide: boolean) {
    const query = this.c.queries.get(id);
    // QUERY_RESULT_AVAILABLE: always; QUERY_RESULT: the value.
    const value = pname === 0x8867 ? 1 : (query?.result ?? 0);
    if (wide) new BigInt64Array(this.memory.u8.buffer, params >>> 0, 1)[0] = BigInt(Math.round(value));
    else this.memory.i32[params >>> 2] = value;
  }

  /** The buffer bound to `target` changed: texel buffers of it are stale. */
  /** Mirrors a write of `size` bytes at `data` to the bound buffer's copy. */
  private shadowWrite(target: number, offset: number, data: number, size: number) {
    const buffer = this.c.boundBuffers.get(target);
    const shadow = buffer && this.c.shadows.get(buffer);
    if (shadow) shadow.set(this.memory.u8.subarray(data, data + size), offset);
  }

  private changed(target: number) {
    const buffer = this.c.boundBuffers.get(target);
    if (!buffer || !this.c.texelBuffers.size) return;
    for (const [info, texel] of this.c.texelBuffers) if (texel.buffer === buffer) this.c.staleTexelBuffers.add(info);
  }

  private trackUniformBuffer(index: number, buffer: WebGLBuffer | null, offset: number, size: number) {
    if (buffer) this.c.uniformBuffers.set(index, { buffer, offset, size });
    else this.c.uniformBuffers.delete(index);
  }

  /** Before a draw: texel buffers up to date, uniform buffer ranges at least
   *  as large as the program's blocks (drivers may round a std140 block's
   *  size up beyond what the game binds, which WebGL rejects). */
  private prepareDraw() {
    this.flushTexelBuffers();
    const c = this.context;
    const program = c?.program;
    if (!c || !program) return;
    const gl = c.gl;
    if (!program.blocks) {
      const count = gl.getProgramParameter(program.object, 0x8a36 /* ACTIVE_UNIFORM_BLOCKS */) as number;
      program.blocks = Array.from({ length: count }, (_, i) => ({
        size: gl.getActiveUniformBlockParameter(program.object, i, 0x8a40 /* UNIFORM_BLOCK_DATA_SIZE */) as number,
        binding: gl.getActiveUniformBlockParameter(program.object, i, 0x8a3f /* UNIFORM_BLOCK_BINDING */) as number,
      }));
    }
    let rebound = false;
    for (const block of program.blocks) {
      const bound = c.uniformBuffers.get(block.binding);
      if (!bound || bound.size < 0 || bound.size >= block.size) continue;
      const available = (c.bufferSizes.get(bound.buffer) ?? Infinity) - bound.offset;
      const size = Math.min(block.size, available);
      if (size > bound.size) {
        gl.bindBufferRange(GL_UNIFORM_BUFFER, block.binding, bound.buffer, bound.offset, size);
        bound.size = size;
        rebound = true;
      }
    }
    // (bindBufferRange also moved the general binding point.)
    if (rebound) gl.bindBuffer(GL_UNIFORM_BUFFER, c.boundBuffers.get(GL_UNIFORM_BUFFER) ?? null);
  }

  private flushTexelBuffers() {
    if (!this.context || !this.c.staleTexelBuffers.size) return;
    for (const info of this.c.staleTexelBuffers) this.uploadTexelBuffer(info);
    this.c.staleTexelBuffers.clear();
  }

  /** Copies a texel buffer's contents into its texture (on the GPU, as a
   *  pixel unpack buffer), TEXEL_BUFFER_WIDTH texels per row. */
  private uploadTexelBuffer(info: TextureInfo) {
    const c = this.c;
    const gl = c.gl;
    const texel = c.texelBuffers.get(info);
    if (!texel) return;
    const [format, type, size] = TEXEL_FORMATS[texel.internal] ?? [GL_RGBA, GL_UNSIGNED_BYTE, 4];
    const count = Math.floor((c.bufferSizes.get(texel.buffer) ?? 0) / size);
    const width = Math.max(1, Math.min(count, TEXEL_BUFFER_WIDTH));
    const height = Math.max(1, Math.ceil(count / width));
    const rows = Math.floor(count / width);
    const rest = count - rows * width;
    const previous = gl.getParameter(0x8069 /* TEXTURE_BINDING_2D */) as WebGLTexture | null;
    for (const pname of [0x0cf2, 0x0cf3, 0x0cf4, 0x806d, 0x806e]) gl.pixelStorei(pname, 0); // row length, skips, image height
    gl.pixelStorei(0x0cf5, 1); // UNPACK_ALIGNMENT
    gl.bindTexture(GL_TEXTURE_2D, info.object);
    gl.texImage2D(GL_TEXTURE_2D, 0, texel.internal, width, height, 0, format, type, null);
    gl.texParameteri(GL_TEXTURE_2D, 0x2801, 0x2600); // MIN_FILTER NEAREST
    gl.texParameteri(GL_TEXTURE_2D, 0x2800, 0x2600); // MAG_FILTER NEAREST
    gl.texParameteri(GL_TEXTURE_2D, 0x813d, 0); // MAX_LEVEL
    if (count > 0) {
      gl.bindBuffer(GL_PIXEL_UNPACK_BUFFER, texel.buffer);
      if (rows > 0) gl.texSubImage2D(GL_TEXTURE_2D, 0, 0, 0, width, rows, format, type, 0);
      if (rest > 0) gl.texSubImage2D(GL_TEXTURE_2D, 0, 0, rows, rest, 1, format, type, rows * width * size);
      gl.bindBuffer(GL_PIXEL_UNPACK_BUFFER, c.boundBuffers.get(GL_PIXEL_UNPACK_BUFFER) ?? null);
    }
    gl.bindTexture(GL_TEXTURE_2D, previous);
    for (const [pname, value] of c.pixelStore) gl.pixelStorei(pname, value);
    gl.pixelStorei(0x0cf5, c.pixelStore.get(0x0cf5) ?? 4);
  }

  private writeLog(text: string, size: number, length: number, log: number) {
    const bytes = new TextEncoder().encode(text);
    const n = Math.max(0, Math.min(bytes.length, size - 1));
    this.memory.u8.set(bytes.subarray(0, n), log >>> 0);
    if (size > 0) this.memory.u8[(log >>> 0) + n] = 0;
    if (length !== 0) this.memory.i32[length >>> 2] = n;
  }
}

/** The time in nanoseconds (GL_TIMESTAMP). */
const nowNs = () => Math.round((performance.timeOrigin + performance.now()) * 1e6);

/** The binding query of a buffer target. */
function bindingQuery(target: number): number {
  switch (target) {
    case 0x8a11: // UNIFORM_BUFFER
      return 0x8a28;
    case 0x8f36: // COPY_READ_BUFFER
      return 0x8f36;
    case 0x8f37: // COPY_WRITE_BUFFER
      return 0x8f37;
    case 0x88eb: // PIXEL_PACK_BUFFER
      return 0x88ed;
    case 0x88ec: // PIXEL_UNPACK_BUFFER
      return 0x88ef;
    default:
      return 0x8894;
  }
}


