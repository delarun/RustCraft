//! OpenGL 3.3 core (no extensions) for LWJGL, on WebGL2.
//!
//! LWJGL's GL natives (`GLxxC.gl*`, liblwjgl_opengl on a desktop) forward to
//! the `rjava_gl` imports (launcher/src/jvm/gl.ts), which run WebGL2 on the
//! current context of this thread and emulate what WebGL2 lacks. Pointers are
//! addresses in the VM's memory, which the JS side reads directly.
//!
//! `proc_address` serves LWJGL's capability detection: GL 3.3 entry points
//! exist (only the few LWJGL calls through pointers are real functions).

use std::ffi::c_char;

use crate::native::*;

/// Declares the `rjava_gl` imports and the natives calling them. Parameter
/// kinds: i int, z boolean, f float, d double, p pointer (long), l
/// size/offset/handle (long, pointer-sized: i32), w 64-bit long.
macro_rules! gl {
    ($($class:literal $name:ident($($arg:ident: $k:ident),*) $ret:ident;)*) => {
        #[cfg_attr(not(rjava_stub), link(wasm_import_module = "rjava_gl"))]
        unsafe extern "C" {
            $(fn $name($($arg: gl!(@ty $k)),*) -> gl!(@ty $ret);)*
        }

        // C entry points (for LWJGL's calls through function addresses, as
        // its Java-array overloads make): the imports' signatures are the
        // C ones under wasm32.
        $(
            #[allow(non_snake_case)]
            mod $name {
                pub(super) extern "C" fn c($($arg: gl!(@ty $k)),*) -> gl!(@ty $ret) {
                    // SAFETY: an import.
                    unsafe { super::$name($($arg),*) }
                }
            }
        )*

        /// (native name, C entry point); the C name drops LWJGL's "n".
        const ENTRY_POINTS: &[(&str, *const ())] = &[$((stringify!($name), $name::c as *const ())),*];

        pub(super) fn register_natives(n: &mut Natives) {
            $(
                n.add(
                    concat!("org/lwjgl/opengl/", $class, ".", stringify!($name), "(", $(gl!(@desc $k),)* ")", gl!(@desc $ret)),
                    #[allow(unused_variables)]
                    |_, a| {
                        super::trace_native(stringify!($name));
                        #[allow(unused_mut, unused_variables)]
                        let mut slot = 0usize;
                        $(let $arg = gl!(@read $k, a, slot);)*
                        // SAFETY: an import.
                        let r = unsafe { $name($($arg),*) };
                        gl!(@ret $ret, r)
                    },
                );
            )*
        }
    };
    (@ty i) => { i32 };
    (@ty z) => { i32 };
    (@ty f) => { f32 };
    (@ty d) => { f64 };
    (@ty p) => { u32 };
    (@ty l) => { i32 };
    (@ty w) => { i64 };
    (@ty v) => { () };
    (@desc i) => { "I" };
    (@desc z) => { "Z" };
    (@desc f) => { "F" };
    (@desc d) => { "D" };
    (@desc p) => { "J" };
    (@desc l) => { "J" };
    (@desc w) => { "J" };
    (@desc v) => { "V" };
    (@read i, $a:ident, $s:ident) => {{ $s += 1; $a[$s - 1] as i32 }};
    (@read z, $a:ident, $s:ident) => {{ $s += 1; ($a[$s - 1] as u8 != 0) as i32 }};
    (@read f, $a:ident, $s:ident) => {{ $s += 1; f32::from_bits($a[$s - 1] as u32) }};
    (@read d, $a:ident, $s:ident) => {{ $s += 2; f64::from_bits($a[$s - 2]) }};
    (@read p, $a:ident, $s:ident) => {{ $s += 2; $a[$s - 2] as u32 }};
    (@read l, $a:ident, $s:ident) => {{ $s += 2; $a[$s - 2] as i64 as i32 }};
    (@read w, $a:ident, $s:ident) => {{ $s += 2; $a[$s - 2] as i64 }};
    (@ret v, $r:ident) => {{ let () = $r; ret_void() }};
    (@ret i, $r:ident) => { ret_i($r) };
    (@ret z, $r:ident) => { ret_z($r != 0) };
    (@ret p, $r:ident) => { ret_j($r as i64) };
    (@ret l, $r:ident) => { ret_j($r as i64) };
}

// Named as LWJGL's natives (which prefix "n" to those taking pointers).
gl! {
    "GL11C" glBindTexture(target: i, texture: i) v;
    "GL11C" glBlendFunc(sfactor: i, dfactor: i) v;
    "GL11C" glClear(mask: i) v;
    "GL11C" glClearColor(r: f, g: f, b: f, a: f) v;
    "GL11C" glClearDepth(depth: d) v;
    "GL11C" glClearStencil(s: i) v;
    "GL11C" glColorMask(r: z, g: z, b: z, a: z) v;
    "GL11C" glCullFace(mode: i) v;
    "GL11C" glDepthFunc(func: i) v;
    "GL11C" glDepthMask(flag: z) v;
    "GL11C" glDisable(cap: i) v;
    "GL11C" glDrawArrays(mode: i, first: i, count: i) v;
    "GL11C" glDrawBuffer(buf: i) v;
    "GL11C" glEnable(cap: i) v;
    "GL11C" glFinish() v;
    "GL11C" glFlush() v;
    "GL11C" glFrontFace(mode: i) v;
    "GL11C" glGetError() i;
    "GL11C" glIsEnabled(cap: i) z;
    "GL11C" glLineWidth(width: f) v;
    "GL11C" glLogicOp(op: i) v;
    "GL11C" glPixelStorei(pname: i, param: i) v;
    "GL11C" glPolygonMode(face: i, mode: i) v;
    "GL11C" glPolygonOffset(factor: f, units: f) v;
    "GL11C" glReadBuffer(src: i) v;
    "GL11C" glScissor(x: i, y: i, w: i, h: i) v;
    "GL11C" glStencilFunc(func: i, reference: i, mask: i) v;
    "GL11C" glStencilMask(mask: i) v;
    "GL11C" glStencilOp(fail: i, zfail: i, zpass: i) v;
    "GL11C" glTexParameterf(target: i, pname: i, param: f) v;
    "GL11C" glTexParameteri(target: i, pname: i, param: i) v;
    "GL11C" glViewport(x: i, y: i, w: i, h: i) v;
    "GL11C" nglDeleteTextures(n: i, textures: p) v;
    "GL11C" nglDrawElements(mode: i, count: i, kind: i, indices: p) v;
    "GL11C" nglGenTextures(n: i, textures: p) v;
    "GL11C" nglGetFloatv(pname: i, params: p) v;
    "GL11C" nglGetIntegerv(pname: i, params: p) v;
    "GL11C" nglGetString(name: i) p;
    "GL11C" nglGetTexImage(target: i, level: i, format: i, kind: i, pixels: p) v;
    "GL11C" nglGetTexLevelParameteriv(target: i, level: i, pname: i, params: p) v;
    "GL11C" nglReadPixels(x: i, y: i, w: i, h: i, format: i, kind: i, pixels: p) v;
    "GL11C" nglTexImage2D(target: i, level: i, internal: i, w: i, h: i, border: i, format: i, kind: i, pixels: p) v;
    "GL11C" nglTexSubImage2D(target: i, level: i, x: i, y: i, w: i, h: i, format: i, kind: i, pixels: p) v;
    "GL12C" nglTexImage3D(target: i, level: i, internal: i, w: i, h: i, d: i, border: i, format: i, kind: i, pixels: p) v;
    "GL12C" nglTexSubImage3D(target: i, level: i, x: i, y: i, z: i, w: i, h: i, d: i, format: i, kind: i, pixels: p) v;
    "GL13C" glActiveTexture(texture: i) v;
    "GL14C" glBlendEquation(mode: i) v;
    "GL14C" glBlendFuncSeparate(srgb: i, drgb: i, salpha: i, dalpha: i) v;
    "GL15C" glBeginQuery(target: i, id: i) v;
    "GL15C" glBindBuffer(target: i, buffer: i) v;
    "GL15C" glEndQuery(target: i) v;
    "GL15C" nglDeleteQueries(n: i, ids: p) v;
    "GL15C" nglGenQueries(n: i, ids: p) v;
    "GL15C" nglGetQueryiv(target: i, pname: i, params: p) v;
    "GL15C" nglGetQueryObjectiv(id: i, pname: i, params: p) v;
    "GL15C" nglGetQueryObjectuiv(id: i, pname: i, params: p) v;
    "GL15C" glUnmapBuffer(target: i) z;
    "GL15C" nglBufferData(target: i, size: l, data: p, usage: i) v;
    "GL15C" nglBufferSubData(target: i, offset: l, size: l, data: p) v;
    "GL15C" nglDeleteBuffers(n: i, buffers: p) v;
    "GL15C" nglGenBuffers(n: i, buffers: p) v;
    "GL15C" nglGetBufferSubData(target: i, offset: l, size: l, data: p) v;
    "GL20C" glAttachShader(program: i, shader: i) v;
    "GL20C" glBlendEquationSeparate(rgb: i, alpha: i) v;
    "GL20C" glCompileShader(shader: i) v;
    "GL20C" glCreateProgram() i;
    "GL20C" glCreateShader(kind: i) i;
    "GL20C" glDeleteProgram(program: i) v;
    "GL20C" glDeleteShader(shader: i) v;
    "GL20C" glDetachShader(program: i, shader: i) v;
    "GL20C" glDisableVertexAttribArray(index: i) v;
    "GL20C" glEnableVertexAttribArray(index: i) v;
    "GL20C" glLinkProgram(program: i) v;
    "GL20C" glUniform1f(location: i, v0: f) v;
    "GL20C" glUniform1i(location: i, v0: i) v;
    "GL20C" glUseProgram(program: i) v;
    "GL20C" nglBindAttribLocation(program: i, index: i, name: p) v;
    "GL20C" nglDrawBuffers(n: i, bufs: p) v;
    "GL20C" nglGetAttribLocation(program: i, name: p) i;
    "GL20C" nglGetProgramInfoLog(program: i, size: i, length: p, log: p) v;
    "GL20C" nglGetProgramiv(program: i, pname: i, params: p) v;
    "GL20C" nglGetShaderInfoLog(shader: i, size: i, length: p, log: p) v;
    "GL20C" nglGetShaderiv(shader: i, pname: i, params: p) v;
    "GL20C" nglGetUniformLocation(program: i, name: p) i;
    "GL20C" nglShaderSource(shader: i, count: i, strings: p, lengths: p) v;
    "GL20C" nglUniform4fv(location: i, count: i, value: p) v;
    "GL20C" nglUniformMatrix4fv(location: i, count: i, transpose: z, value: p) v;
    "GL20C" nglVertexAttribPointer(index: i, size: i, kind: i, normalized: z, stride: i, pointer: p) v;
    "GL30C" glBindBufferBase(target: i, index: i, buffer: i) v;
    "GL30C" glBindBufferRange(target: i, index: i, buffer: i, offset: l, size: l) v;
    "GL30C" glBindFramebuffer(target: i, framebuffer: i) v;
    "GL30C" glBindRenderbuffer(target: i, renderbuffer: i) v;
    "GL30C" glBindVertexArray(array: i) v;
    "GL30C" glBlitFramebuffer(sx0: i, sy0: i, sx1: i, sy1: i, dx0: i, dy0: i, dx1: i, dy1: i, mask: i, filter: i) v;
    "GL30C" glCheckFramebufferStatus(target: i) i;
    "GL30C" glDisablei(target: i, index: i) v;
    "GL30C" glEnablei(target: i, index: i) v;
    "GL30C" glFlushMappedBufferRange(target: i, offset: l, length: l) v;
    "GL30C" glFramebufferRenderbuffer(target: i, attachment: i, rbtarget: i, renderbuffer: i) v;
    "GL30C" glFramebufferTexture2D(target: i, attachment: i, textarget: i, texture: i, level: i) v;
    "GL30C" glFramebufferTextureLayer(target: i, attachment: i, texture: i, level: i, layer: i) v;
    "GL30C" glGenerateMipmap(target: i) v;
    "GL30C" glRenderbufferStorage(target: i, internal: i, w: i, h: i) v;
    "GL30C" glVertexAttribI4i(index: i, x: i, y: i, z: i, w: i) v;
    "GL30C" nglClearBufferfv(buffer: i, drawbuffer: i, value: p) v;
    "GL30C" nglClearBufferiv(buffer: i, drawbuffer: i, value: p) v;
    "GL30C" nglDeleteFramebuffers(n: i, framebuffers: p) v;
    "GL30C" nglDeleteRenderbuffers(n: i, renderbuffers: p) v;
    "GL30C" nglDeleteVertexArrays(n: i, arrays: p) v;
    "GL30C" nglGenFramebuffers(n: i, framebuffers: p) v;
    "GL30C" nglGenRenderbuffers(n: i, renderbuffers: p) v;
    "GL30C" nglGenVertexArrays(n: i, arrays: p) v;
    "GL30C" nglGetStringi(name: i, index: i) p;
    "GL30C" nglMapBufferRange(target: i, offset: l, length: l, access: i) p;
    "GL30C" nglVertexAttribIPointer(index: i, size: i, kind: i, stride: i, pointer: p) v;
    "GL31C" glCopyBufferSubData(read: i, write: i, roffset: l, woffset: l, size: l) v;
    "GL31C" glDrawArraysInstanced(mode: i, first: i, count: i, instances: i) v;
    "GL31C" glTexBuffer(target: i, internal: i, buffer: i) v;
    "GL31C" glUniformBlockBinding(program: i, index: i, binding: i) v;
    "GL31C" nglDrawElementsInstanced(mode: i, count: i, kind: i, indices: p, instances: i) v;
    "GL31C" nglGetUniformBlockIndex(program: i, name: p) i;
    "GL32C" glDeleteSync(sync: l) v;
    "GL32C" glFenceSync(condition: i, flags: i) l;
    "GL32C" nglClientWaitSync(sync: l, flags: i, timeout: w) i;
    "GL32C" nglDrawElementsBaseVertex(mode: i, count: i, kind: i, indices: p, base: i) v;
    "GL32C" nglDrawElementsInstancedBaseVertex(mode: i, count: i, kind: i, indices: p, instances: i, base: i) v;
    "GL33C" glBindSampler(unit: i, sampler: i) v;
    "GL33C" glQueryCounter(id: i, target: i) v;
    "GL33C" nglGetQueryObjecti64v(id: i, pname: i, params: p) v;
    "GL33C" nglGetQueryObjectui64v(id: i, pname: i, params: p) v;
    "GL33C" glSamplerParameterf(sampler: i, pname: i, param: f) v;
    "GL33C" glSamplerParameteri(sampler: i, pname: i, param: i) v;
    "GL33C" glVertexAttribDivisor(index: i, divisor: i) v;
    "GL33C" nglDeleteSamplers(n: i, samplers: p) v;
    "GL33C" nglGenSamplers(n: i, samplers: p) v;
}

/// LWJGL's capability detection calls these through their addresses.
extern "C" fn get_string(name: i32) -> u32 {
    unsafe { nglGetString(name) }
}
extern "C" fn get_stringi(name: i32, index: i32) -> u32 {
    unsafe { nglGetStringi(name, index) }
}
extern "C" fn get_integerv(pname: i32, params: u32) {
    unsafe { nglGetIntegerv(pname, params) }
}
extern "C" fn get_error() -> i32 {
    0
}
pub(super) fn not_callable_address() -> usize {
    not_callable as *const () as usize
}

/// Stands for entry points only called through natives.
extern "C" fn not_callable() {
    panic!("a GL function was called through its address");
}

/// GL entry points (`glXGetProcAddress`, `SDL_GL_GetProcAddress`): GL 3.3
/// core exists; extensions do not.
pub(super) fn proc_address(name: &str) -> usize {
    let f: *const () = match name {
        "glGetString" => get_string as *const (),
        "glGetStringi" => get_stringi as *const (),
        "glGetIntegerv" => get_integerv as *const (),
        "glGetError" => get_error as *const (),
        n if n.starts_with("gl") && !has_vendor_suffix(n) => ENTRY_POINTS
            .iter()
            .find(|(native, _)| native.strip_prefix('n').unwrap_or(native) == n)
            .map_or(not_callable as *const (), |&(_, f)| f),
        _ => return 0,
    };
    super::libs::remember(f as usize, if f == not_callable as *const () { "(a GL function only callable as a native)" } else { name });
    f as usize
}

fn has_vendor_suffix(name: &str) -> bool {
    ["ARB", "EXT", "KHR", "NV", "NVX", "AMD", "ATI", "APPLE", "INTEL", "MESA", "OES", "SGIS", "SGIX", "SUN", "IBM", "OVR", "QCOM", "3DFX", "GREMEDY", "HP", "S3", "WIN", "PGI", "INGR", "MESAX", "OML", "SGI"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// libGL / libGLX: the GLX entry points LWJGL looks up.
pub(super) fn glx_symbol(name: &str) -> Option<usize> {
    Some(match name {
        "glXGetProcAddress" | "glXGetProcAddressARB" => glx_get_proc_address as *const () as usize,
        n => match proc_address(n) {
            0 => return None,
            address => address,
        },
    })
}

extern "C" fn glx_get_proc_address(name: *const c_char) -> usize {
    proc_address(super::c_str(name as u32))
}

pub(super) fn register(n: &mut Natives) {
    register_natives(n);
}
