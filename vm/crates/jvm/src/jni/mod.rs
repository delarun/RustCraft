//! JNI, natively: native libraries loaded with dlopen, `native` methods bound
//! to their `Java_...` symbols and called through libffi, and the JNIEnv /
//! JavaVM interfaces those libraries use.
//!
//! References handed to native code are pointers to slots holding a `Ref`:
//! local references live in a per-thread table and are dropped when the
//! native method returns, global ones in a VM-wide table. Both are roots
//! that pin their objects (native code may keep raw pointers into arrays).
//! While a thread runs native code it is outside the running set, so the
//! collector does not wait for it (SDL's event loop, a vsync swap...); JNI
//! functions re-enter the running set while they touch the heap.

// The JNI functions take types private to this module.
#![allow(private_interfaces)]
// Function tables are built from `fn as usize`.
#![allow(function_casts_as_integer)]

mod names;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Mutex, OnceLock};

use libffi::middle::{Arg, Cif, CodePtr, Type};

use crate::class::{ClassRef, FieldKind, MethodRef};
use crate::heap::{ARRAY_BASE_OFFSET, Ref};
use crate::interp::{JResult, Thread, Throw};
use crate::vm::Vm;

const JNI_VERSION: i32 = 0x0018_0000; // JNI_VERSION_24
const JNI_OK: i32 = 0;
const JNI_ERR: i32 = -1;
const JNI_EDETACHED: i32 = -2;
const LOCAL_CAPACITY: usize = 1 << 16;

type JObject = *mut c_void;
type Env = *mut EnvBlock;

/// Per-VM JNI state: loaded libraries, registered natives, global references.
#[derive(Default)]
pub struct Jni {
    handles: Mutex<Vec<usize>>,
    /// RegisterNatives: native key → function.
    registered: Mutex<HashMap<String, usize>>,
    globals: Globals,
    field_ids: Mutex<HashMap<(u32, String, String), usize>>,
    java_vm: OnceLock<usize>,
}

/// Global references: fixed slots (their addresses are the references).
#[derive(Default)]
struct Globals {
    chunks: Mutex<Vec<&'static [std::sync::atomic::AtomicU32]>>,
    free: Mutex<Vec<usize>>,
}

const GLOBAL_CHUNK: usize = 4096;

impl Globals {
    fn add(&self, r: Ref) -> JObject {
        if r.is_null() {
            return std::ptr::null_mut();
        }
        let mut free = self.free.lock().unwrap();
        let slot = match free.pop() {
            Some(slot) => slot,
            None => {
                let mut chunks = self.chunks.lock().unwrap();
                let chunk: &'static [std::sync::atomic::AtomicU32] =
                    Box::leak((0..GLOBAL_CHUNK).map(|_| std::sync::atomic::AtomicU32::new(0)).collect());
                let base = chunks.len() * GLOBAL_CHUNK;
                chunks.push(chunk);
                free.extend((base + 1..base + GLOBAL_CHUNK).rev());
                base
            }
        };
        let cell = &self.chunks.lock().unwrap()[slot / GLOBAL_CHUNK][slot % GLOBAL_CHUNK];
        cell.store(r.0, std::sync::atomic::Ordering::Relaxed);
        cell as *const _ as JObject
    }

    fn remove(&self, obj: JObject) {
        let chunks = self.chunks.lock().unwrap();
        for (i, chunk) in chunks.iter().enumerate() {
            let start = chunk.as_ptr() as usize;
            let address = obj as usize;
            if (start..start + GLOBAL_CHUNK * 4).contains(&address) {
                chunk[(address - start) / 4].store(0, std::sync::atomic::Ordering::Relaxed);
                self.free.lock().unwrap().push(i * GLOBAL_CHUNK + (address - start) / 4);
                return;
            }
        }
    }

    fn visit(&self, f: &mut dyn FnMut(Ref)) {
        for chunk in self.chunks.lock().unwrap().iter() {
            for cell in chunk.iter() {
                let r = cell.load(std::sync::atomic::Ordering::Relaxed);
                if r != 0 {
                    f(Ref(r));
                }
            }
        }
    }
}

impl Jni {
    /// System.load: dlopen and JNI_OnLoad. Called from a Java thread, which
    /// leaves the running set for the native code.
    pub fn load(&self, t: &mut Thread<'_>, path: &str) -> Result<usize, String> {
        let c_path = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: dlopen of a path; the library's initialisers run.
        let handle = t.blocking(|| unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) });
        if handle.is_null() {
            // SAFETY: dlerror returns a C string or null.
            let error = unsafe { libc::dlerror() };
            let message = if error.is_null() { "dlopen failed".to_owned() } else { unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned() };
            return Err(message);
        }
        self.handles.lock().unwrap().push(handle as usize);
        if let Some(on_load) = symbol(handle as usize, "JNI_OnLoad") {
            let java_vm = self.java_vm(t.vm);
            // SAFETY: JNI_OnLoad(JavaVM*, void*) -> jint.
            let f: extern "C" fn(*mut c_void, *mut c_void) -> i32 = unsafe { std::mem::transmute(on_load) };
            let version = t.blocking(|| f(java_vm as *mut c_void, std::ptr::null_mut()));
            if version < 0 || version > JNI_VERSION {
                return Err(format!("{path}: unsupported JNI version {version:#x}"));
            }
        }
        Ok(handle as usize)
    }

    /// The `Java_...` function for a native method, if a loaded library has it.
    pub fn find_native(&self, method: MethodRef) -> Option<usize> {
        let key = method.native_key();
        if let Some(&f) = self.registered.lock().unwrap().get(&key) {
            return Some(f);
        }
        let short = format!("Java_{}_{}", mangle(&method.class().name), mangle(&method.name));
        let args = &method.descriptor[1..method.descriptor.find(')').unwrap_or(1)];
        let long = format!("{short}__{}", mangle(args));
        let handles = self.handles.lock().unwrap().clone();
        handles.iter().find_map(|&h| symbol(h, &short).or_else(|| symbol(h, &long)))
    }

    /// GC roots: global references (pinned).
    pub(crate) fn visit_globals(&self, f: &mut dyn FnMut(Ref)) {
        self.globals.visit(f);
    }

    fn java_vm(&self, vm: &Vm) -> usize {
        *self.java_vm.get_or_init(|| {
            let table: &'static [usize; 8] = Box::leak(Box::new([
                0,
                0,
                0,
                destroy_java_vm as usize,
                attach_current_thread as usize,
                detach_current_thread as usize,
                get_env as usize,
                attach_current_thread as usize,
            ]));
            let block: &'static mut VmBlock = Box::leak(Box::new(VmBlock { functions: table.as_ptr(), vm: vm.as_static() }));
            block as *mut VmBlock as usize
        })
    }
}

/// `dlsym`.
pub fn symbol(handle: usize, name: &str) -> Option<usize> {
    let name = CString::new(name).ok()?;
    // SAFETY: dlsym on a handle from dlopen.
    let f = unsafe { libc::dlsym(handle as *mut c_void, name.as_ptr()) };
    (!f.is_null()).then_some(f as usize)
}

/// JNI name mangling (JNI spec, "Resolving Native Method Names").
fn mangle(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        match c {
            '/' => out.push('_'),
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("_0{:04x}", u));
                }
            }
        }
    }
    out
}

#[repr(C)]
struct VmBlock {
    functions: *const usize,
    vm: &'static Vm,
}

// --- per-thread state -----------------------------------------------------------

/// A Java thread's JNI state. `env` is what native code gets as JNIEnv*.
#[repr(C)]
pub struct JniThread {
    env: EnvBlock,
    locals: Box<[u32]>,
    top: usize,
    /// Exception thrown by native code (Throw), until the native returns.
    pending: Ref,
    /// Class loaders of the native methods running (FindClass uses the top).
    loaders: Vec<Ref>,
    frames: Vec<usize>,
}

#[repr(C)]
struct EnvBlock {
    functions: *const usize,
    thread: *mut Thread<'static>,
}

impl JniThread {
    pub(crate) fn new(thread: *mut Thread<'static>) -> Box<JniThread> {
        Box::new(JniThread {
            env: EnvBlock { functions: table().as_ptr(), thread },
            locals: vec![0u32; LOCAL_CAPACITY].into_boxed_slice(),
            top: 0,
            pending: Ref::NULL,
            loaders: Vec::new(),
            frames: Vec::new(),
        })
    }

    /// GC roots: local references and the pending exception (pinned).
    pub(crate) fn visit(&self, f: &mut dyn FnMut(u64, bool)) {
        for &slot in &self.locals[..self.top] {
            if slot != 0 {
                f(slot as u64, true);
            }
        }
        f(self.pending.slot(), true);
    }

    fn local(&mut self, r: Ref) -> JObject {
        if r.is_null() {
            return std::ptr::null_mut();
        }
        assert!(self.top < self.locals.len(), "JNI local reference table overflow");
        self.locals[self.top] = r.0;
        self.top += 1;
        &mut self.locals[self.top - 1] as *mut u32 as JObject
    }
}

fn deref(obj: JObject) -> Ref {
    if obj.is_null() { Ref::NULL } else { Ref(unsafe { (obj as *const u32).read() }) }
}

/// The Thread of a JNIEnv (the first field of the JniThread).
fn thread_of<'a>(env: Env) -> &'a mut Thread<'static> {
    // SAFETY: env points at the EnvBlock of a live JniThread.
    unsafe { &mut *(*env).thread }
}

fn jni_of<'a>(t: &'a mut Thread<'static>) -> &'a mut JniThread {
    t.jni_state()
}

/// Runs `f` inside the running set (JNI functions touch the heap).
fn with<R>(env: Env, f: impl FnOnce(&mut Thread<'static>) -> R) -> R {
    let t = thread_of(env);
    t.vm.threads.safepoints.enter();
    let result = f(t);
    t.vm.threads.safepoints.leave();
    result
}

/// Records a Java exception for the native code's caller.
fn pend(t: &mut Thread<'static>, Throw(exception): Throw) {
    jni_of(t).pending = exception;
}

// --- calling native methods --------------------------------------------------------

/// A bound native method: its function and libffi call interface.
pub struct JniMethod {
    code: usize,
    cif: Cif,
}

// SAFETY: the Cif is immutable after creation.
unsafe impl Send for JniMethod {}
unsafe impl Sync for JniMethod {}

fn ffi_type(kind: Option<FieldKind>) -> Type {
    match kind {
        None => Type::void(),
        Some(FieldKind::Boolean) => Type::u8(),
        Some(FieldKind::Byte) => Type::i8(),
        Some(FieldKind::Char) => Type::u16(),
        Some(FieldKind::Short) => Type::i16(),
        Some(FieldKind::Int) => Type::i32(),
        Some(FieldKind::Long) => Type::i64(),
        Some(FieldKind::Float) => Type::f32(),
        Some(FieldKind::Double) => Type::f64(),
        Some(FieldKind::Ref) => Type::pointer(),
    }
}

impl JniMethod {
    pub fn new(method: MethodRef, code: usize) -> JniMethod {
        let mut args = vec![Type::pointer(), Type::pointer()];
        args.extend(method.params.iter().map(|&k| ffi_type(Some(k))));
        JniMethod { code, cif: Cif::new(args, ffi_type(method.ret)) }
    }
}

enum Value {
    P(usize),
    Z(u8),
    B(i8),
    C(u16),
    S(i16),
    I(i32),
    J(i64),
    F(f32),
    D(f64),
}

impl Value {
    fn arg(&self) -> Arg<'_> {
        match self {
            Value::P(v) => Arg::new(v),
            Value::Z(v) => Arg::new(v),
            Value::B(v) => Arg::new(v),
            Value::C(v) => Arg::new(v),
            Value::S(v) => Arg::new(v),
            Value::I(v) => Arg::new(v),
            Value::J(v) => Arg::new(v),
            Value::F(v) => Arg::new(v),
            Value::D(v) => Arg::new(v),
        }
    }
}

/// RJAVA_JNI_TRACE=<file>: counts calls of JNI-bound methods and, for
/// LWJGL's `JNI.invoke*`/`call*` (which take the C function's address last),
/// of the C functions by symbol; the counts are rewritten to the file every
/// five seconds; first calls are logged. (What the browser build must provide.)
mod trace {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static COUNTS: OnceLock<Option<Mutex<HashMap<String, u64>>>> = OnceLock::new();

    fn counts() -> Option<&'static Mutex<HashMap<String, u64>>> {
        COUNTS
            .get_or_init(|| {
                let path = std::env::var_os("RJAVA_JNI_TRACE")?;
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    let mut lines: Vec<(String, u64)> =
                        counts().unwrap().lock().unwrap().iter().map(|(k, &v)| (k.clone(), v)).collect();
                    lines.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    let text: String = lines.iter().map(|(name, n)| format!("{n}\t{name}\n")).collect();
                    let _ = std::fs::write(&path, text);
                });
                Some(Mutex::new(HashMap::new()))
            })
            .as_ref()
    }

    pub fn record(method: crate::class::MethodRef, args: &[u64]) {
        let Some(counts) = counts() else { return };
        let mut key = format!("{}.{}{}", method.class().name, method.name, method.descriptor);
        if method.class().name == "org/lwjgl/system/JNI" && method.descriptor.contains("J)") {
            let address = args[args.len() - 2] as usize;
            // SAFETY: dladdr only reads its arguments.
            let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
            if address != 0 && unsafe { libc::dladdr(address as *const libc::c_void, &mut info) } != 0 && !info.dli_sname.is_null() {
                // SAFETY: a NUL-terminated symbol name from dladdr.
                key = format!("C {}", unsafe { std::ffi::CStr::from_ptr(info.dli_sname) }.to_string_lossy());
            } else {
                key = format!("C ?{address:#x}");
            }
        }
        let mut counts = counts.lock().unwrap();
        if !counts.contains_key(&key) {
            eprintln!("[jni] first call: {key}");
        }
        *counts.entry(key).or_default() += 1;
    }
}

/// Calls a native method bound to a JNI function; `args` are its slots.
pub(crate) fn call(t: &mut Thread<'_>, method: MethodRef, jni: &JniMethod, args: &[u64]) -> JResult<Option<u64>> {
    trace::record(method, args);
    // SAFETY: Thread<'vm> with the VM's 'static lifetime (the VM is leaked).
    let t: &mut Thread<'static> = unsafe { std::mem::transmute(t) };
    if method.is_static() {
        t.ensure_initialized(method.class())?;
    }
    let receiver = if method.is_static() { t.vm.mirror(method.class()) } else { Ref::from_slot(args[0]) };
    let loader = method.class().loader;
    let state = jni_of(t);
    let frame = state.top;
    state.loaders.push(loader);
    let env = &mut state.env as *mut EnvBlock as usize;
    let mut values = vec![Value::P(env), Value::P(state.local(receiver) as usize)];
    let mut slot = if method.is_static() { 0 } else { 1 };
    for &kind in &method.params {
        let v = args[slot];
        values.push(match kind {
            FieldKind::Boolean => Value::Z(v as u8),
            FieldKind::Byte => Value::B(v as i8),
            FieldKind::Char => Value::C(v as u16),
            FieldKind::Short => Value::S(v as i16),
            FieldKind::Int => Value::I(v as i32),
            FieldKind::Long => Value::J(v as i64),
            FieldKind::Float => Value::F(f32::from_bits(v as u32)),
            FieldKind::Double => Value::D(f64::from_bits(v)),
            FieldKind::Ref => Value::P(state.local(Ref::from_slot(v)) as usize),
        });
        slot += if kind.is_wide() { 2 } else { 1 };
    }
    let ffi_args: Vec<Arg> = values.iter().map(Value::arg).collect();
    let code = CodePtr(jni.code as *mut c_void);
    // Native code runs outside the running set.
    t.vm.threads.safepoints.leave();
    // SAFETY: the function has the JNI signature the Cif describes.
    let raw: u64 = unsafe {
        match method.ret {
            Some(FieldKind::Float) => jni.cif.call::<f32>(code, &ffi_args).to_bits() as u64,
            Some(FieldKind::Double) => jni.cif.call::<f64>(code, &ffi_args).to_bits(),
            _ => jni.cif.call::<u64>(code, &ffi_args),
        }
    };
    t.vm.threads.safepoints.enter();
    let result = match method.ret {
        None => None,
        Some(FieldKind::Boolean) => Some((raw as u8 != 0) as u64),
        Some(FieldKind::Byte) => Some(raw as i8 as i32 as u32 as u64),
        Some(FieldKind::Char) => Some(raw as u16 as u64),
        Some(FieldKind::Short) => Some(raw as i16 as i32 as u32 as u64),
        Some(FieldKind::Int) | Some(FieldKind::Float) => Some(raw as u32 as u64),
        Some(FieldKind::Long) | Some(FieldKind::Double) => Some(raw),
        Some(FieldKind::Ref) => {
            let r = deref(raw as usize as JObject);
            crate::threads::push_handle(r);
            Some(r.slot())
        }
    };
    let state = jni_of(t);
    state.top = frame;
    state.loaders.pop();
    let pending = std::mem::replace(&mut state.pending, Ref::NULL);
    if !pending.is_null() {
        crate::threads::push_handle(pending);
        return Err(Throw(pending));
    }
    Ok(result)
}

// --- the JavaVM interface ---------------------------------------------------------------

extern "C" fn destroy_java_vm(_vm: *mut c_void) -> i32 {
    JNI_ERR
}

extern "C" fn get_env(_vm: *mut c_void, penv: *mut *mut c_void, _version: i32) -> i32 {
    match crate::interp::current_thread() {
        Some(t) => {
            let state = t.jni_state();
            // SAFETY: penv is a valid out pointer.
            unsafe { *penv = &mut state.env as *mut EnvBlock as *mut c_void };
            JNI_OK
        }
        None => {
            // SAFETY: as above.
            unsafe { *penv = std::ptr::null_mut() };
            JNI_EDETACHED
        }
    }
}

extern "C" fn attach_current_thread(vm: *mut c_void, penv: *mut *mut c_void, args: *mut c_void) -> i32 {
    if get_env(vm, penv, JNI_VERSION) == JNI_OK {
        return JNI_OK;
    }
    let _ = args;
    eprintln!("rjava: JNI AttachCurrentThread from a foreign thread is not supported yet");
    JNI_ERR
}

extern "C" fn detach_current_thread(_vm: *mut c_void) -> i32 {
    JNI_OK
}

// --- the JNIEnv function table --------------------------------------------------------------

/// Unimplemented functions report themselves (by slot) and abort.
extern "C" fn unimplemented<const SLOT: usize>() {
    eprintln!("rjava: JNI function {} is not implemented", names::NAMES[SLOT]);
    std::process::abort();
}

macro_rules! stubs {
    ($($i:literal)*) => {
        [$(unimplemented::<$i> as usize),*]
    };
}

fn table() -> &'static [usize; names::NAMES.len()] {
    static TABLE: OnceLock<[usize; names::NAMES.len()]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table: [usize; names::NAMES.len()] = stubs!(
            0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39
            40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79
            80 81 82 83 84 85 86 87 88 89 90 91 92 93 94 95 96 97 98 99 100 101 102 103 104 105 106 107 108 109 110 111 112 113 114 115 116 117 118 119
            120 121 122 123 124 125 126 127 128 129 130 131 132 133 134 135 136 137 138 139 140 141 142 143 144 145 146 147 148 149 150 151 152 153 154 155 156 157 158 159
            160 161 162 163 164 165 166 167 168 169 170 171 172 173 174 175 176 177 178 179 180 181 182 183 184 185 186 187 188 189 190 191 192 193 194 195 196 197 198 199
            200 201 202 203 204 205 206 207 208 209 210 211 212 213 214 215 216 217 218 219 220 221 222 223 224 225 226 227 228 229 230 231 232 233 234 235
        );
        for slot in 0..4 {
            table[slot] = 0; // reserved
        }
        for (name, f) in implemented() {
            let slot = names::NAMES.iter().position(|n| *n == name).unwrap_or_else(|| panic!("JNI function {name}"));
            table[slot] = f;
        }
        table
    })
}

/// Arguments of a Call*Method: from a jvalue array, a va_list or the
/// registers of a variadic call.
enum CallArgs {
    Array(*const u64),
    VaList(*mut VaList),
    Variadic(Variadic),
}

/// x86-64 System V va_list.
#[repr(C)]
struct VaList {
    gp_offset: u32,
    fp_offset: u32,
    overflow: *mut u64,
    save_area: *mut u8,
}

/// The variable part of a variadic call: the integer registers left after
/// the fixed arguments, the vector registers, and the stack.
struct Variadic {
    ints: [u64; 3],
    /// Integer registers holding variadic arguments (2 or 3).
    int_regs: usize,
    floats: [f64; 8],
    stack: [u64; 16],
    next_int: usize,
    next_float: usize,
    next_stack: usize,
}

impl CallArgs {
    /// The next argument of `kind`, as an operand slot.
    fn next(&mut self, kind: FieldKind, index: usize) -> u64 {
        let float = matches!(kind, FieldKind::Float | FieldKind::Double);
        let raw: u64 = match self {
            CallArgs::Array(values) => unsafe { values.add(index).read() },
            CallArgs::VaList(list) => unsafe {
                let list = &mut **list;
                if float && list.fp_offset < 176 {
                    let v = (list.save_area.add(list.fp_offset as usize) as *const u64).read();
                    list.fp_offset += 16;
                    v
                } else if !float && list.gp_offset < 48 {
                    let v = (list.save_area.add(list.gp_offset as usize) as *const u64).read();
                    list.gp_offset += 8;
                    v
                } else {
                    let v = list.overflow.read();
                    list.overflow = list.overflow.add(1);
                    v
                }
            },
            CallArgs::Variadic(v) => {
                if float && v.next_float < 8 {
                    v.next_float += 1;
                    v.floats[v.next_float - 1].to_bits()
                } else if !float && v.next_int < v.int_regs {
                    v.next_int += 1;
                    v.ints[v.next_int - 1]
                } else {
                    v.next_stack += 1;
                    v.stack[v.next_stack - 1]
                }
            }
        };
        let jvalue = matches!(self, CallArgs::Array(_));
        match kind {
            // In a jvalue array a float is a float; in varargs it is promoted to double.
            FieldKind::Float if !jvalue => (f64::from_bits(raw) as f32).to_bits() as u64,
            FieldKind::Float => raw as u32 as u64,
            FieldKind::Ref => deref(raw as usize as JObject).slot(),
            FieldKind::Long | FieldKind::Double => raw,
            FieldKind::Boolean => (raw as u8 != 0) as u64,
            FieldKind::Byte => raw as i8 as i32 as u32 as u64,
            FieldKind::Char => raw as u16 as u64,
            FieldKind::Short => raw as i16 as i32 as u32 as u64,
            FieldKind::Int => raw as u32 as u64,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Dispatch {
    Virtual,
    Nonvirtual,
    Static,
}

/// Call*Method: invokes `method` (a jmethodID) and returns the result slot
/// (0 if it threw; the exception is pending).
fn call_method(env: Env, target: JObject, method: *const crate::class::Method, dispatch: Dispatch, mut args: CallArgs) -> u64 {
    with(env, |t| {
        // SAFETY: jmethodIDs are Method pointers.
        let method: MethodRef = unsafe { &*method };
        let mut slots = Vec::with_capacity(method.arg_slots as usize);
        let mut target_method = method;
        if dispatch != Dispatch::Static {
            let receiver = deref(target);
            if receiver.is_null() {
                let e = t.throw_new("java/lang/NullPointerException", None);
                pend(t, e);
                return 0;
            }
            if dispatch == Dispatch::Virtual {
                match t.vm.select_method(t.vm.class_of(receiver), method) {
                    Ok(selected) => target_method = selected,
                    Err(e) => {
                        let e = t.throw_vm_error(e);
                        pend(t, e);
                        return 0;
                    }
                }
            }
            slots.push(receiver.slot());
        }
        for (i, &kind) in method.params.iter().enumerate() {
            slots.push(args.next(kind, i));
            if kind.is_wide() {
                slots.push(0);
            }
        }
        match t.invoke(target_method, &slots) {
            Ok(Some(v)) if method.ret == Some(FieldKind::Ref) => jni_of(t).local(Ref::from_slot(v)) as usize as u64,
            Ok(v) => v.unwrap_or(0),
            Err(e) => {
                pend(t, e);
                0
            }
        }
    })
}

macro_rules! call_functions {
    ($($kind:ident $ty:ty => $conv:expr),* $(,)?) => {
        $(
            paste_call!($kind, $ty, $conv);
        )*
    };
}

macro_rules! paste_call {
    ($kind:ident, $ty:ty, $conv:expr) => {
        #[allow(non_snake_case, clippy::too_many_arguments)]
        pub(super) mod $kind {
            use super::*;
            fn convert(v: u64) -> $ty {
                ($conv)(v)
            }
            fn variadic(ints: [u64; 3], floats: [f64; 8], stack: [u64; 16]) -> CallArgs {
                CallArgs::Variadic(Variadic { ints, int_regs: 3, floats, stack, next_int: 0, next_float: 0, next_stack: 0 })
            }
            pub(crate) extern "C" fn call(env: Env, obj: JObject, m: *const crate::class::Method, i0: u64, i1: u64, i2: u64,
                f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
                s0: u64, s1: u64, s2: u64, s3: u64, s4: u64, s5: u64, s6: u64, s7: u64,
                s8: u64, s9: u64, s10: u64, s11: u64, s12: u64, s13: u64, s14: u64, s15: u64) -> $ty {
                let args = variadic([i0, i1, i2], [f0, f1, f2, f3, f4, f5, f6, f7], [s0, s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11, s12, s13, s14, s15]);
                convert(call_method(env, obj, m, Dispatch::Virtual, args))
            }
            pub(crate) extern "C" fn call_v(env: Env, obj: JObject, m: *const crate::class::Method, list: *mut VaList) -> $ty {
                convert(call_method(env, obj, m, Dispatch::Virtual, CallArgs::VaList(list)))
            }
            pub(crate) extern "C" fn call_a(env: Env, obj: JObject, m: *const crate::class::Method, args: *const u64) -> $ty {
                convert(call_method(env, obj, m, Dispatch::Virtual, CallArgs::Array(args)))
            }
            pub(crate) extern "C" fn nonvirtual(env: Env, obj: JObject, _class: JObject, m: *const crate::class::Method, i0: u64, i1: u64,
                f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
                s0: u64, s1: u64, s2: u64, s3: u64, s4: u64, s5: u64, s6: u64, s7: u64,
                s8: u64, s9: u64, s10: u64, s11: u64, s12: u64, s13: u64, s14: u64, s15: u64) -> $ty {
                // Two integer registers are left after (env, obj, class, method).
                let mut args = variadic([i0, i1, 0], [f0, f1, f2, f3, f4, f5, f6, f7], [s0, s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11, s12, s13, s14, s15]);
                if let CallArgs::Variadic(v) = &mut args {
                    v.int_regs = 2;
                }
                convert(call_method(env, obj, m, Dispatch::Nonvirtual, args))
            }
            pub(crate) extern "C" fn nonvirtual_v(env: Env, obj: JObject, _class: JObject, m: *const crate::class::Method, list: *mut VaList) -> $ty {
                convert(call_method(env, obj, m, Dispatch::Nonvirtual, CallArgs::VaList(list)))
            }
            pub(crate) extern "C" fn nonvirtual_a(env: Env, obj: JObject, _class: JObject, m: *const crate::class::Method, args: *const u64) -> $ty {
                convert(call_method(env, obj, m, Dispatch::Nonvirtual, CallArgs::Array(args)))
            }
            pub(crate) extern "C" fn static_call(env: Env, class: JObject, m: *const crate::class::Method, i0: u64, i1: u64, i2: u64,
                f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
                s0: u64, s1: u64, s2: u64, s3: u64, s4: u64, s5: u64, s6: u64, s7: u64,
                s8: u64, s9: u64, s10: u64, s11: u64, s12: u64, s13: u64, s14: u64, s15: u64) -> $ty {
                let args = variadic([i0, i1, i2], [f0, f1, f2, f3, f4, f5, f6, f7], [s0, s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11, s12, s13, s14, s15]);
                convert(call_method(env, class, m, Dispatch::Static, args))
            }
            pub(crate) extern "C" fn static_v(env: Env, class: JObject, m: *const crate::class::Method, list: *mut VaList) -> $ty {
                convert(call_method(env, class, m, Dispatch::Static, CallArgs::VaList(list)))
            }
            pub(crate) extern "C" fn static_a(env: Env, class: JObject, m: *const crate::class::Method, args: *const u64) -> $ty {
                convert(call_method(env, class, m, Dispatch::Static, CallArgs::Array(args)))
            }
        }
    };
}

call_functions! {
    object JObject => |v: u64| v as usize as JObject,
    boolean u8 => |v: u64| v as u8,
    byte i8 => |v: u64| v as i8,
    char u16 => |v: u64| v as u16,
    short i16 => |v: u64| v as i16,
    int i32 => |v: u64| v as i32,
    long i64 => |v: u64| v as i64,
    float f32 => |v: u64| f32::from_bits(v as u32),
    double f64 => |v: u64| f64::from_bits(v),
    void () => |_v: u64| (),
}

// --- other functions ----------------------------------------------------------------------

extern "C" fn get_version(_env: Env) -> i32 {
    JNI_VERSION
}

fn class_of_ref(t: &Thread<'static>, class: JObject) -> ClassRef {
    t.vm.class_from_mirror(deref(class))
}

extern "C" fn find_class(env: Env, name: *const c_char) -> JObject {
    with(env, |t| {
        // SAFETY: a C string.
        let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        let loader = jni_of(t).loaders.last().copied().unwrap_or(Ref::NULL);
        let loader = if loader.is_null() {
            match t.system_class_loader() {
                Ok(l) => l,
                Err(e) => {
                    pend(t, e);
                    return std::ptr::null_mut();
                }
            }
        } else {
            loader
        };
        match t.load_class_in(loader, &name) {
            Ok(class) => {
                let mirror = t.vm.mirror(class);
                jni_of(t).local(mirror)
            }
            Err(e) => {
                pend(t, e);
                std::ptr::null_mut()
            }
        }
    })
}

extern "C" fn get_superclass(env: Env, class: JObject) -> JObject {
    with(env, |t| match class_of_ref(t, class).super_class {
        Some(s) if !class_of_ref(t, class).is_interface() => {
            let m = t.vm.mirror(s);
            jni_of(t).local(m)
        }
        _ => std::ptr::null_mut(),
    })
}

extern "C" fn is_assignable_from(env: Env, from: JObject, to: JObject) -> u8 {
    with(env, |t| class_of_ref(t, from).is_subclass_of(class_of_ref(t, to)) as u8)
}

extern "C" fn throw(env: Env, obj: JObject) -> i32 {
    with(env, |t| {
        jni_of(t).pending = deref(obj);
        JNI_OK
    })
}

extern "C" fn throw_new(env: Env, class: JObject, message: *const c_char) -> i32 {
    with(env, |t| {
        let class = class_of_ref(t, class);
        let message = (!message.is_null()).then(|| unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned());
        let e = t.throw_new(&class.name, message.as_deref());
        pend(t, e);
        JNI_OK
    })
}

extern "C" fn exception_occurred(env: Env) -> JObject {
    with(env, |t| {
        let pending = jni_of(t).pending;
        jni_of(t).local(pending)
    })
}

extern "C" fn exception_describe(env: Env) {
    with(env, |t| {
        let pending = jni_of(t).pending;
        if !pending.is_null() {
            eprintln!("JNI exception: {}", t.vm.class_of(pending).java_name());
        }
    })
}

extern "C" fn exception_clear(env: Env) {
    with(env, |t| jni_of(t).pending = Ref::NULL)
}

extern "C" fn exception_check(env: Env) -> u8 {
    with(env, |t| (!jni_of(t).pending.is_null()) as u8)
}

extern "C" fn fatal_error(_env: Env, message: *const c_char) {
    eprintln!("JNI FatalError: {}", unsafe { CStr::from_ptr(message) }.to_string_lossy());
    std::process::abort();
}

extern "C" fn push_local_frame(env: Env, _capacity: i32) -> i32 {
    with(env, |t| {
        let state = jni_of(t);
        state.frames.push(state.top);
        JNI_OK
    })
}

extern "C" fn pop_local_frame(env: Env, result: JObject) -> JObject {
    with(env, |t| {
        let r = deref(result);
        let state = jni_of(t);
        if let Some(top) = state.frames.pop() {
            state.top = top;
        }
        state.local(r)
    })
}

extern "C" fn new_global_ref(env: Env, obj: JObject) -> JObject {
    with(env, |t| t.vm.jni.globals.add(deref(obj)))
}

extern "C" fn delete_global_ref(env: Env, obj: JObject) {
    if !obj.is_null() {
        with(env, |t| t.vm.jni.globals.remove(obj))
    }
}

extern "C" fn delete_local_ref(_env: Env, obj: JObject) {
    if !obj.is_null() {
        // SAFETY: a local reference slot (dropping it just stops rooting).
        unsafe { (obj as *mut u32).write(0) };
    }
}

extern "C" fn is_same_object(_env: Env, a: JObject, b: JObject) -> u8 {
    (deref(a) == deref(b)) as u8
}

extern "C" fn new_local_ref(env: Env, obj: JObject) -> JObject {
    with(env, |t| {
        let r = deref(obj);
        jni_of(t).local(r)
    })
}

extern "C" fn ensure_local_capacity(_env: Env, _capacity: i32) -> i32 {
    JNI_OK
}

extern "C" fn alloc_object(env: Env, class: JObject) -> JObject {
    with(env, |t| {
        let class = class_of_ref(t, class);
        match t.vm.new_object(class) {
            Ok(obj) => jni_of(t).local(obj),
            Err(e) => {
                let e = t.throw_vm_error(e);
                pend(t, e);
                std::ptr::null_mut()
            }
        }
    })
}

/// NewObject*: allocate, then run the constructor (a Call*Method).
fn new_object(env: Env, class: JObject, m: *const crate::class::Method, args: CallArgs) -> JObject {
    let obj = alloc_object(env, class);
    if obj.is_null() {
        return obj;
    }
    call_method(env, obj, m, Dispatch::Nonvirtual, args);
    obj
}

extern "C" fn new_object_a(env: Env, class: JObject, m: *const crate::class::Method, args: *const u64) -> JObject {
    new_object(env, class, m, CallArgs::Array(args))
}

extern "C" fn new_object_v(env: Env, class: JObject, m: *const crate::class::Method, list: *mut VaList) -> JObject {
    new_object(env, class, m, CallArgs::VaList(list))
}

#[allow(clippy::too_many_arguments)]
extern "C" fn new_object_variadic(env: Env, class: JObject, m: *const crate::class::Method, i0: u64, i1: u64, i2: u64,
    f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
    s0: u64, s1: u64, s2: u64, s3: u64, s4: u64, s5: u64, s6: u64, s7: u64) -> JObject {
    let args = CallArgs::Variadic(Variadic {
        ints: [i0, i1, i2],
        int_regs: 3,
        floats: [f0, f1, f2, f3, f4, f5, f6, f7],
        stack: [s0, s1, s2, s3, s4, s5, s6, s7, 0, 0, 0, 0, 0, 0, 0, 0],
        next_int: 0,
        next_float: 0,
        next_stack: 0,
    });
    new_object(env, class, m, args)
}

extern "C" fn get_object_class(env: Env, obj: JObject) -> JObject {
    with(env, |t| {
        let class = t.vm.class_of(deref(obj));
        let mirror = t.vm.mirror(class);
        jni_of(t).local(mirror)
    })
}

extern "C" fn is_instance_of(env: Env, obj: JObject, class: JObject) -> u8 {
    with(env, |t| {
        let r = deref(obj);
        (r.is_null() || t.vm.class_of(r).is_subclass_of(class_of_ref(t, class))) as u8
    })
}

fn find_method_in(class: ClassRef, name: &str, sig: &str) -> Option<MethodRef> {
    class
        .super_classes()
        .find_map(|c| c.find_method(name, sig))
        .or_else(|| class.all_interfaces().into_iter().find_map(|i| i.find_method(name, sig)))
}

fn method_id(env: Env, class: JObject, name: *const c_char, sig: *const c_char, is_static: bool) -> *const crate::class::Method {
    with(env, |t| {
        let class = class_of_ref(t, class);
        // SAFETY: C strings.
        let (name, sig) = unsafe { (CStr::from_ptr(name).to_string_lossy().into_owned(), CStr::from_ptr(sig).to_string_lossy().into_owned()) };
        if let Err(e) = t.ensure_initialized(class) {
            pend(t, e);
            return std::ptr::null();
        }
        match find_method_in(class, &name, &sig).filter(|m| m.is_static() == is_static) {
            Some(m) => m as *const crate::class::Method,
            None => {
                let e = t.throw_new("java/lang/NoSuchMethodError", Some(&format!("{}.{name}{sig}", class.name)));
                pend(t, e);
                std::ptr::null()
            }
        }
    })
}

extern "C" fn get_method_id(env: Env, class: JObject, name: *const c_char, sig: *const c_char) -> *const crate::class::Method {
    method_id(env, class, name, sig, false)
}

extern "C" fn get_static_method_id(env: Env, class: JObject, name: *const c_char, sig: *const c_char) -> *const crate::class::Method {
    method_id(env, class, name, sig, true)
}

/// A jfieldID.
struct FieldId {
    holder: ClassRef,
    offset: u32,
    kind: FieldKind,
    is_static: bool,
}

fn field_id(env: Env, class: JObject, name: *const c_char, sig: *const c_char, is_static: bool) -> *const FieldId {
    with(env, |t| {
        let class = class_of_ref(t, class);
        // SAFETY: C strings.
        let (name, sig) = unsafe { (CStr::from_ptr(name).to_string_lossy().into_owned(), CStr::from_ptr(sig).to_string_lossy().into_owned()) };
        if let Err(e) = t.ensure_initialized(class) {
            pend(t, e);
            return std::ptr::null();
        }
        let key = (class.id, name.clone(), sig.clone());
        if let Some(&id) = t.vm.jni.field_ids.lock().unwrap().get(&key) {
            return id as *const FieldId;
        }
        let found = class.super_classes().find_map(|c| c.find_field(&name, &sig).filter(|f| f.is_static() == is_static).map(|f| (c, f)));
        let found = found.or_else(|| {
            class.all_interfaces().iter().copied().find_map(|i| i.find_field(&name, &sig).filter(|f| f.is_static() == is_static).map(|f| (i, f)))
        });
        match found {
            Some((holder, field)) => {
                let id = Box::leak(Box::new(FieldId { holder, offset: field.offset, kind: field.kind, is_static }));
                t.vm.jni.field_ids.lock().unwrap().insert(key, id as *const FieldId as usize);
                id
            }
            None => {
                let e = t.throw_new("java/lang/NoSuchFieldError", Some(&name));
                pend(t, e);
                std::ptr::null()
            }
        }
    })
}

extern "C" fn get_field_id(env: Env, class: JObject, name: *const c_char, sig: *const c_char) -> *const FieldId {
    field_id(env, class, name, sig, false)
}

extern "C" fn get_static_field_id(env: Env, class: JObject, name: *const c_char, sig: *const c_char) -> *const FieldId {
    field_id(env, class, name, sig, true)
}

fn field_get(env: Env, obj: JObject, id: *const FieldId) -> u64 {
    with(env, |t| {
        // SAFETY: jfieldIDs are leaked FieldIds.
        let id = unsafe { &*id };
        let holder = if id.is_static { id.holder.statics.get() } else { deref(obj) };
        let v = crate::interp::read_field(&t.vm.heap, holder, id.offset, id.kind);
        if id.kind == FieldKind::Ref { jni_of(t).local(Ref::from_slot(v)) as usize as u64 } else { v }
    })
}

fn field_set(env: Env, obj: JObject, id: *const FieldId, value: u64) {
    with(env, |t| {
        // SAFETY: as above.
        let id = unsafe { &*id };
        let holder = if id.is_static { id.holder.statics.get() } else { deref(obj) };
        let value = if id.kind == FieldKind::Ref { deref(value as usize as JObject).slot() } else { value };
        crate::interp::write_field(&t.vm.heap, holder, id.offset, id.kind, value);
    })
}

macro_rules! field_functions {
    ($($kind:ident $ty:ty => |$v:ident| $from:expr, |$x:ident| $to:expr);* $(;)?) => {
        $(
            #[allow(non_snake_case)]
            pub(super) mod $kind {
                use super::*;
                pub(crate) extern "C" fn get(env: Env, obj: JObject, id: *const FieldId) -> $ty { let $v = field_get(env, obj, id); $from }
                pub(crate) extern "C" fn set(env: Env, obj: JObject, id: *const FieldId, $x: $ty) { field_set(env, obj, id, $to) }
            }
        )*
    };
}

mod fields {
    use super::*;
    field_functions! {
        object JObject => |v| v as usize as JObject, |x| x as usize as u64;
        boolean u8 => |v| v as u8, |x| x as u64;
        byte i8 => |v| v as i8, |x| x as i32 as u32 as u64;
        char u16 => |v| v as u16, |x| x as u64;
        short i16 => |v| v as i16, |x| x as i32 as u32 as u64;
        int i32 => |v| v as i32, |x| x as u32 as u64;
        long i64 => |v| v as i64, |x| x as u64;
        float f32 => |v| f32::from_bits(v as u32), |x| x.to_bits() as u64;
        double f64 => |v| f64::from_bits(v), |x| x.to_bits();
    }
}

extern "C" fn new_string(env: Env, chars: *const u16, len: i32) -> JObject {
    with(env, |t| {
        // SAFETY: len UTF-16 units.
        let units = unsafe { std::slice::from_raw_parts(chars, len.max(0) as usize) };
        match t.vm.new_string_utf16(units) {
            Ok(s) => jni_of(t).local(s),
            Err(e) => {
                let e = t.throw_vm_error(e);
                pend(t, e);
                std::ptr::null_mut()
            }
        }
    })
}

extern "C" fn get_string_length(env: Env, s: JObject) -> i32 {
    with(env, |t| t.vm.string_utf16(deref(s)).len() as i32)
}

/// GetStringChars/GetStringCritical: a malloc'd copy (released by Release*).
extern "C" fn get_string_chars(env: Env, s: JObject, is_copy: *mut u8) -> *const u16 {
    with(env, |t| {
        let units = t.vm.string_utf16(deref(s));
        if !is_copy.is_null() {
            unsafe { *is_copy = 1 };
        }
        // SAFETY: a malloc'd buffer of the right size.
        unsafe {
            let p = libc::malloc((units.len() + 1) * 2) as *mut u16;
            std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
            *p.add(units.len()) = 0;
            p
        }
    })
}

extern "C" fn release_chars(_env: Env, _s: JObject, chars: *const c_void) {
    // SAFETY: from get_string_chars / get_string_utf_chars.
    unsafe { libc::free(chars as *mut c_void) };
}

extern "C" fn new_string_utf(env: Env, utf: *const c_char) -> JObject {
    with(env, |t| {
        if utf.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: a C string.
        let bytes = unsafe { CStr::from_ptr(utf) }.to_bytes();
        let units = classfile::mutf8::decode_utf16(bytes).unwrap_or_else(|_| String::from_utf8_lossy(bytes).encode_utf16().collect());
        match t.vm.new_string_utf16(&units) {
            Ok(s) => jni_of(t).local(s),
            Err(e) => {
                let e = t.throw_vm_error(e);
                pend(t, e);
                std::ptr::null_mut()
            }
        }
    })
}

extern "C" fn get_string_utf_length(env: Env, s: JObject) -> i32 {
    with(env, |t| classfile::mutf8::encode_utf16(&t.vm.string_utf16(deref(s))).len() as i32)
}

extern "C" fn get_string_utf_length_as_long(env: Env, s: JObject) -> i64 {
    get_string_utf_length(env, s) as i64
}

extern "C" fn get_string_utf_chars(env: Env, s: JObject, is_copy: *mut u8) -> *const c_char {
    with(env, |t| {
        let bytes = classfile::mutf8::encode_utf16(&t.vm.string_utf16(deref(s)));
        if !is_copy.is_null() {
            unsafe { *is_copy = 1 };
        }
        // SAFETY: a malloc'd NUL-terminated copy.
        unsafe {
            let p = libc::malloc(bytes.len() + 1) as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            *p.add(bytes.len()) = 0;
            p as *const c_char
        }
    })
}

extern "C" fn get_string_region(env: Env, s: JObject, start: i32, len: i32, buf: *mut u16) {
    with(env, |t| {
        let units = t.vm.string_utf16(deref(s));
        let (start, len) = (start.max(0) as usize, len.max(0) as usize);
        if start + len <= units.len() {
            // SAFETY: the caller's buffer holds len units.
            unsafe { std::ptr::copy_nonoverlapping(units[start..].as_ptr(), buf, len) };
        }
    })
}

extern "C" fn get_string_utf_region(env: Env, s: JObject, start: i32, len: i32, buf: *mut u8) {
    with(env, |t| {
        let units = t.vm.string_utf16(deref(s));
        let (start, len) = (start.max(0) as usize, len.max(0) as usize);
        if start + len <= units.len() {
            let bytes = classfile::mutf8::encode_utf16(&units[start..start + len]);
            // SAFETY: the caller's buffer (sized by GetStringUTFLength).
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
                *buf.add(bytes.len()) = 0;
            }
        }
    })
}

extern "C" fn get_array_length(env: Env, array: JObject) -> i32 {
    with(env, |t| t.vm.heap.array_length(deref(array)))
}

extern "C" fn new_object_array(env: Env, len: i32, class: JObject, init: JObject) -> JObject {
    with(env, |t| {
        let component = class_of_ref(t, class);
        let array_class = t.vm.array_class(component);
        match t.vm.new_array(array_class, len) {
            Ok(array) => {
                let init = deref(init);
                if !init.is_null() {
                    for i in 0..len.max(0) as u32 {
                        t.vm.heap.write(array, ARRAY_BASE_OFFSET + i * 4, init.0);
                    }
                }
                jni_of(t).local(array)
            }
            Err(e) => {
                let e = t.throw_vm_error(e);
                pend(t, e);
                std::ptr::null_mut()
            }
        }
    })
}

extern "C" fn get_object_array_element(env: Env, array: JObject, index: i32) -> JObject {
    with(env, |t| {
        let array = deref(array);
        if index < 0 || index >= t.vm.heap.array_length(array) {
            let e = t.throw_new("java/lang/ArrayIndexOutOfBoundsException", Some(&index.to_string()));
            pend(t, e);
            return std::ptr::null_mut();
        }
        let r = Ref(t.vm.heap.read(array, ARRAY_BASE_OFFSET + index as u32 * 4));
        jni_of(t).local(r)
    })
}

extern "C" fn set_object_array_element(env: Env, array: JObject, index: i32, value: JObject) {
    with(env, |t| {
        let array = deref(array);
        if index < 0 || index >= t.vm.heap.array_length(array) {
            let e = t.throw_new("java/lang/ArrayIndexOutOfBoundsException", Some(&index.to_string()));
            pend(t, e);
            return;
        }
        t.vm.heap.write(array, ARRAY_BASE_OFFSET + index as u32 * 4, deref(value).0);
    })
}

fn new_primitive_array(env: Env, kind: FieldKind, len: i32) -> JObject {
    with(env, |t| match t.vm.new_primitive_array(kind, len) {
        Ok(array) => jni_of(t).local(array),
        Err(e) => {
            let e = t.throw_vm_error(e);
            pend(t, e);
            std::ptr::null_mut()
        }
    })
}

/// Get<Type>ArrayElements / GetPrimitiveArrayCritical: the array's own data
/// (arrays referenced from native code are pinned).
extern "C" fn get_array_elements(env: Env, array: JObject, is_copy: *mut u8) -> *mut c_void {
    with(env, |t| {
        if !is_copy.is_null() {
            unsafe { *is_copy = 0 };
        }
        t.vm.heap.ptr(deref(array), ARRAY_BASE_OFFSET) as *mut c_void
    })
}

extern "C" fn release_array_elements(_env: Env, _array: JObject, _elems: *mut c_void, _mode: i32) {}

fn array_region(env: Env, array: JObject, start: i32, len: i32, buf: *mut u8, size: usize, store: bool) {
    with(env, |t| {
        let array = deref(array);
        let length = t.vm.heap.array_length(array);
        if start < 0 || len < 0 || start as i64 + len as i64 > length as i64 {
            let e = t.throw_new("java/lang/ArrayIndexOutOfBoundsException", None);
            pend(t, e);
            return;
        }
        let data = t.vm.heap.ptr(array, ARRAY_BASE_OFFSET + start as u32 * size as u32);
        // SAFETY: bounds checked; the caller's buffer holds len elements.
        unsafe {
            if store {
                std::ptr::copy_nonoverlapping(buf, data, len as usize * size);
            } else {
                std::ptr::copy_nonoverlapping(data, buf, len as usize * size);
            }
        }
    })
}

macro_rules! array_functions {
    ($($kind:ident $fk:expr, $size:expr);* $(;)?) => {
        $(
            pub(super) mod $kind {
                use super::*;
                pub(crate) extern "C" fn new(env: Env, len: i32) -> JObject { new_primitive_array(env, $fk, len) }
                pub(crate) extern "C" fn get_region(env: Env, a: JObject, start: i32, len: i32, buf: *mut u8) { array_region(env, a, start, len, buf, $size, false) }
                pub(crate) extern "C" fn set_region(env: Env, a: JObject, start: i32, len: i32, buf: *mut u8) { array_region(env, a, start, len, buf, $size, true) }
            }
        )*
    };
}

mod arrays {
    use super::*;
    array_functions! {
        boolean FieldKind::Boolean, 1;
        byte FieldKind::Byte, 1;
        char FieldKind::Char, 2;
        short FieldKind::Short, 2;
        int FieldKind::Int, 4;
        long FieldKind::Long, 8;
        float FieldKind::Float, 4;
        double FieldKind::Double, 8;
    }
}

#[repr(C)]
struct NativeMethod {
    name: *const c_char,
    signature: *const c_char,
    function: *mut c_void,
}

extern "C" fn register_natives(env: Env, class: JObject, methods: *const NativeMethod, count: i32) -> i32 {
    with(env, |t| {
        let class = class_of_ref(t, class);
        for i in 0..count.max(0) as usize {
            // SAFETY: count entries with C strings.
            let m = unsafe { &*methods.add(i) };
            let (name, sig) = unsafe { (CStr::from_ptr(m.name).to_string_lossy(), CStr::from_ptr(m.signature).to_string_lossy()) };
            let key = format!("{}.{}{}", class.name, name, sig);
            t.vm.jni.registered.lock().unwrap().insert(key, m.function as usize);
        }
        JNI_OK
    })
}

extern "C" fn unregister_natives(_env: Env, _class: JObject) -> i32 {
    JNI_OK
}

extern "C" fn monitor_enter(env: Env, obj: JObject) -> i32 {
    with(env, |t| match t.monitor_enter(deref(obj)) {
        Ok(()) => JNI_OK,
        Err(e) => {
            pend(t, e);
            JNI_ERR
        }
    })
}

extern "C" fn monitor_exit(env: Env, obj: JObject) -> i32 {
    with(env, |t| match t.monitor_exit(deref(obj)) {
        Ok(()) => JNI_OK,
        Err(e) => {
            pend(t, e);
            JNI_ERR
        }
    })
}

extern "C" fn get_java_vm(env: Env, out: *mut *mut c_void) -> i32 {
    let t = thread_of(env);
    // SAFETY: an out pointer.
    unsafe { *out = t.vm.jni.java_vm(t.vm) as *mut c_void };
    JNI_OK
}

extern "C" fn new_direct_byte_buffer(env: Env, address: *mut c_void, capacity: i64) -> JObject {
    with(env, |t| {
        let result = (|| -> JResult<Ref> {
            let class = t.load_class("java/nio/DirectByteBuffer")?;
            t.ensure_initialized(class)?;
            let buffer = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
            crate::threads::push_handle(buffer);
            let init = class.find_method("<init>", "(JJ)V").expect("DirectByteBuffer(long, long)");
            t.invoke(init, &[buffer.slot(), address as u64, 0, capacity as u64, 0])?;
            Ok(buffer)
        })();
        match result {
            Ok(buffer) => jni_of(t).local(buffer),
            Err(e) => {
                pend(t, e);
                std::ptr::null_mut()
            }
        }
    })
}

extern "C" fn get_direct_buffer_address(env: Env, buffer: JObject) -> *mut c_void {
    with(env, |t| t.field(deref(buffer), "java/nio/Buffer", "address") as usize as *mut c_void)
}

extern "C" fn get_direct_buffer_capacity(env: Env, buffer: JObject) -> i64 {
    with(env, |t| t.field(deref(buffer), "java/nio/Buffer", "capacity") as i32 as i64)
}

extern "C" fn get_object_ref_type(_env: Env, obj: JObject) -> i32 {
    if obj.is_null() { 0 } else { 1 } // JNILocalRefType (close enough)
}

extern "C" fn from_reflected_method(env: Env, method: JObject) -> *const crate::class::Method {
    with(env, |t| {
        let method = deref(method);
        let class = t.vm.class_of(method);
        let (holder_field, slot_field) = if class.name == "java/lang/reflect/Constructor" {
            ("java/lang/reflect/Constructor", "java/lang/reflect/Constructor")
        } else {
            ("java/lang/reflect/Method", "java/lang/reflect/Method")
        };
        let holder = t.vm.class_from_mirror(t.field_ref(method, holder_field, "clazz"));
        holder.find_method_by_slot(t.field(method, slot_field, "slot") as usize) as *const crate::class::Method
    })
}

/// (name in the JNI table, function) for everything implemented.
fn implemented() -> Vec<(&'static str, usize)> {
    let mut f: Vec<(&'static str, usize)> = vec![
        ("GetVersion", get_version as usize),
        ("FindClass", find_class as usize),
        ("FromReflectedMethod", from_reflected_method as usize),
        ("GetSuperclass", get_superclass as usize),
        ("IsAssignableFrom", is_assignable_from as usize),
        ("Throw", throw as usize),
        ("ThrowNew", throw_new as usize),
        ("ExceptionOccurred", exception_occurred as usize),
        ("ExceptionDescribe", exception_describe as usize),
        ("ExceptionClear", exception_clear as usize),
        ("FatalError", fatal_error as usize),
        ("PushLocalFrame", push_local_frame as usize),
        ("PopLocalFrame", pop_local_frame as usize),
        ("NewGlobalRef", new_global_ref as usize),
        ("DeleteGlobalRef", delete_global_ref as usize),
        ("DeleteLocalRef", delete_local_ref as usize),
        ("IsSameObject", is_same_object as usize),
        ("NewLocalRef", new_local_ref as usize),
        ("EnsureLocalCapacity", ensure_local_capacity as usize),
        ("AllocObject", alloc_object as usize),
        ("NewObject", new_object_variadic as usize),
        ("NewObjectV", new_object_v as usize),
        ("NewObjectA", new_object_a as usize),
        ("GetObjectClass", get_object_class as usize),
        ("IsInstanceOf", is_instance_of as usize),
        ("GetMethodID", get_method_id as usize),
        ("GetStaticMethodID", get_static_method_id as usize),
        ("GetFieldID", get_field_id as usize),
        ("GetStaticFieldID", get_static_field_id as usize),
        ("NewString", new_string as usize),
        ("GetStringLength", get_string_length as usize),
        ("GetStringChars", get_string_chars as usize),
        ("ReleaseStringChars", release_chars as usize),
        ("NewStringUTF", new_string_utf as usize),
        ("GetStringUTFLength", get_string_utf_length as usize),
        ("GetStringUTFLengthAsLong", get_string_utf_length_as_long as usize),
        ("GetStringUTFChars", get_string_utf_chars as usize),
        ("ReleaseStringUTFChars", release_chars as usize),
        ("GetStringRegion", get_string_region as usize),
        ("GetStringUTFRegion", get_string_utf_region as usize),
        ("GetStringCritical", get_string_chars as usize),
        ("ReleaseStringCritical", release_chars as usize),
        ("GetArrayLength", get_array_length as usize),
        ("NewObjectArray", new_object_array as usize),
        ("GetObjectArrayElement", get_object_array_element as usize),
        ("SetObjectArrayElement", set_object_array_element as usize),
        ("GetPrimitiveArrayCritical", get_array_elements as usize),
        ("ReleasePrimitiveArrayCritical", release_array_elements as usize),
        ("RegisterNatives", register_natives as usize),
        ("UnregisterNatives", unregister_natives as usize),
        ("MonitorEnter", monitor_enter as usize),
        ("MonitorExit", monitor_exit as usize),
        ("GetJavaVM", get_java_vm as usize),
        ("NewDirectByteBuffer", new_direct_byte_buffer as usize),
        ("GetDirectBufferAddress", get_direct_buffer_address as usize),
        ("GetDirectBufferCapacity", get_direct_buffer_capacity as usize),
        ("GetObjectRefType", get_object_ref_type as usize),
        ("NewWeakGlobalRef", new_global_ref as usize),
        ("DeleteWeakGlobalRef", delete_global_ref as usize),
        ("ExceptionCheck", exception_check as usize),
    ];
    macro_rules! calls {
        ($($kind:ident $name:literal),*) => {
            $(
                f.push((concat!("Call", $name, "Method"), $kind::call as usize));
                f.push((concat!("Call", $name, "MethodV"), $kind::call_v as usize));
                f.push((concat!("Call", $name, "MethodA"), $kind::call_a as usize));
                f.push((concat!("CallNonvirtual", $name, "Method"), $kind::nonvirtual as usize));
                f.push((concat!("CallNonvirtual", $name, "MethodV"), $kind::nonvirtual_v as usize));
                f.push((concat!("CallNonvirtual", $name, "MethodA"), $kind::nonvirtual_a as usize));
                f.push((concat!("CallStatic", $name, "Method"), $kind::static_call as usize));
                f.push((concat!("CallStatic", $name, "MethodV"), $kind::static_v as usize));
                f.push((concat!("CallStatic", $name, "MethodA"), $kind::static_a as usize));
            )*
        };
    }
    calls!(object "Object", boolean "Boolean", byte "Byte", char "Char", short "Short", int "Int", long "Long", float "Float", double "Double", void "Void");
    macro_rules! field_access {
        ($($kind:ident $name:literal),*) => {
            $(
                f.push((concat!("Get", $name, "Field"), fields::$kind::get as usize));
                f.push((concat!("Set", $name, "Field"), fields::$kind::set as usize));
                f.push((concat!("GetStatic", $name, "Field"), fields::$kind::get as usize));
                f.push((concat!("SetStatic", $name, "Field"), fields::$kind::set as usize));
            )*
        };
    }
    field_access!(object "Object", boolean "Boolean", byte "Byte", char "Char", short "Short", int "Int", long "Long", float "Float", double "Double");
    macro_rules! array_access {
        ($($kind:ident $name:literal),*) => {
            $(
                f.push((concat!("New", $name, "Array"), arrays::$kind::new as usize));
                f.push((concat!("Get", $name, "ArrayElements"), get_array_elements as usize));
                f.push((concat!("Release", $name, "ArrayElements"), release_array_elements as usize));
                f.push((concat!("Get", $name, "ArrayRegion"), arrays::$kind::get_region as usize));
                f.push((concat!("Set", $name, "ArrayRegion"), arrays::$kind::set_region as usize));
            )*
        };
    }
    array_access!(boolean "Boolean", byte "Byte", char "Char", short "Short", int "Int", long "Long", float "Float", double "Double");
    f
}
