//! The browser platform (wasm builds): what LWJGL's native libraries do on
//! a desktop, implemented in the VM.
//!
//! There is no dynamic linking in the browser: `dlopen` knows a fixed set of
//! libraries (`libs`), and a C function's "address" is its index in the
//! module's indirect function table, so LWJGL's `JNI.invoke*` natives
//! (`invoke`, generated) call it with `call_indirect`. The functions are
//! Rust (`extern "C"`) or C/C++ compiled into the VM; pointers are 32-bit
//! addresses in the VM's memory, which also holds the Java heap.

mod al;
mod ffi;
mod gl;
mod invoke;
mod libs;
mod lwjgl;
mod sdl;
mod shaders;
mod stb;

use std::cell::Cell;
use std::sync::Mutex;

use crate::heap::Ref;
use crate::interp::Thread;
use crate::native::Natives;

pub(crate) fn register(n: &mut Natives) {
    invoke::register(n);
    lwjgl::register(n);
    ffi::register(n);
    gl::register(n);
    stb::register(n);
}

thread_local! {
    /// The interpreter thread inside a native call on this thread, for
    /// callbacks from C into Java.
    static CURRENT: Cell<*mut Thread<'static>> = const { Cell::new(std::ptr::null_mut()) };
}

/// Runs a native function with `t` available to callbacks (`with_current`).
pub(crate) fn enter_native<R>(t: &mut Thread<'_>, f: impl FnOnce() -> R) -> R {
    let previous = CURRENT.replace(t as *mut Thread<'_> as *mut Thread<'static>);
    let result = f();
    CURRENT.set(previous);
    result
}

/// The thread whose native call is running here (C calling back into Java).
pub(crate) fn with_current<R>(f: impl FnOnce(&mut Thread<'static>) -> R) -> R {
    let t = CURRENT.get();
    assert!(!t.is_null(), "a callback into Java outside a native call");
    // SAFETY: set by `enter_native` on this thread, which is still running
    // the native call that led here.
    f(unsafe { &mut *t })
}

// Support for the generated `invoke` natives.
pub(crate) mod invoke_support {
    pub(super) use super::enter_native as call;
    use crate::heap::{ARRAY_BASE_OFFSET, Ref};
    use crate::interp::Thread;

    /// A function pointer from its address (a table index); throws for
    /// addresses of functions the browser build does not provide.
    ///
    /// # Safety
    /// `F` must be the function's type.
    pub(super) unsafe fn function<F: Copy>(t: &mut Thread<'_>, address: u64) -> crate::interp::JResult<F> {
        if address == 0 || address > u32::MAX as u64 || super::libs::is_placeholder(address as usize) {
            let message = format!("native function at {address:#x} is not available in the browser");
            return Err(t.throw_new("java/lang/UnsatisfiedLinkError", Some(&message)));
        }
        super::trace_first_call(address as usize);
        // SAFETY: function pointers are table indices (usize) under wasm32.
        Ok(unsafe { std::mem::transmute_copy(&(address as usize)) })
    }

    /// The address of a Java array's elements (0 for null).
    pub(crate) fn array(t: &Thread<'_>, slot: u64) -> u32 {
        let array = Ref::from_slot(slot);
        if array.is_null() { 0 } else { t.vm.heap.ptr(array, ARRAY_BASE_OFFSET) as u32 }
    }
}

/// RJAVA_WEB_TRACE: logs the first call of each C function (by the name
/// dlsym found it under) and of each GL native.
pub(crate) fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RJAVA_WEB_TRACE").is_some())
}

static TRACED: Mutex<Option<std::collections::HashSet<usize>>> = Mutex::new(None);

pub(crate) fn trace_first_call(address: usize) {
    if tracing() && TRACED.lock().unwrap().get_or_insert_with(Default::default).insert(address) {
        eprintln!("[web] first call: {}", libs::name_of(address).unwrap_or_else(|| format!("{address:#x}")));
    }
}

pub(crate) fn trace_native(name: &'static str) {
    if tracing() && TRACED.lock().unwrap().get_or_insert_with(Default::default).insert(name.as_ptr() as usize) {
        eprintln!("[web] first call: {name}");
    }
}

/// JNI global references: GC roots (pinned, so the referents stay put),
/// named by index + 1.
static GLOBALS: Mutex<(Vec<Ref>, Vec<usize>)> = Mutex::new((Vec::new(), Vec::new()));

pub(crate) fn new_global(r: Ref) -> u64 {
    if r.is_null() {
        return 0;
    }
    let mut globals = GLOBALS.lock().unwrap();
    let (refs, free) = &mut *globals;
    let index = match free.pop() {
        Some(index) => {
            refs[index] = r;
            index
        }
        None => {
            refs.push(r);
            refs.len() - 1
        }
    };
    index as u64 + 1
}

pub(crate) fn delete_global(handle: u64) {
    if handle == 0 {
        return;
    }
    let mut globals = GLOBALS.lock().unwrap();
    let (refs, free) = &mut *globals;
    refs[handle as usize - 1] = Ref::NULL;
    free.push(handle as usize - 1);
}

pub(crate) fn global(handle: u64) -> Ref {
    if handle == 0 { Ref::NULL } else { GLOBALS.lock().unwrap().0[handle as usize - 1] }
}

/// For the collector: the global references' referents.
pub(crate) fn visit_globals(f: &mut dyn FnMut(Ref)) {
    for &r in &GLOBALS.lock().unwrap().0 {
        if !r.is_null() {
            f(r);
        }
    }
}

/// The NUL-terminated string at `address`.
pub(crate) fn c_str<'a>(address: u32) -> &'a str {
    if address == 0 {
        return "";
    }
    // SAFETY: callers pass C strings in the VM's memory.
    let bytes = unsafe { std::ffi::CStr::from_ptr(address as usize as *const std::ffi::c_char) };
    bytes.to_str().unwrap_or("")
}

// The C library's allocator (wasi-libc), which Rust's allocator also uses.
unsafe extern "C" {
    pub(crate) fn malloc(size: usize) -> *mut std::ffi::c_void;
    pub(crate) fn calloc(count: usize, size: usize) -> *mut std::ffi::c_void;
    pub(crate) fn realloc(pointer: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void;
    pub(crate) fn free(pointer: *mut std::ffi::c_void);
    pub(crate) fn aligned_alloc(alignment: usize, size: usize) -> *mut std::ffi::c_void;
}

/// A leaked C string (for results that stay valid).
pub(crate) fn leak_c_str(s: &str) -> u32 {
    let bytes: &'static mut [u8] = Box::leak(format!("{s}\0").into_bytes().into_boxed_slice());
    bytes.as_ptr() as u32
}
