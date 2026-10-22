//! Browser JIT backend: the VM itself runs as WebAssembly (rjava.wasm in Web
//! Workers), and compiled methods become further modules instantiated through
//! the JS API on the VM's own shared memory (see launcher/src/jvm/jit.ts).
//!
//! `load` only keeps a module's bytes, in the VM's memory where every thread
//! can see them; each worker compiles and instantiates a module the first
//! time it runs it, and keeps its own `vm.table` of `call` functions. The
//! modules' imports are this crate's exported `rjava_jit_*` functions, which
//! find the running interpreter thread in a thread local.

#![cfg(target_family = "wasm")]

use std::cell::Cell;
use std::sync::RwLock;

use jvm::jit::JitBackend;

#[cfg_attr(not(rjava_stub), link(wasm_import_module = "rjava_jit"))]
unsafe extern "C" {
    /// Instantiates module `module` (bytes at `wasm[..len]`, for method
    /// `method`, calling methods below `table_size`) in this worker; returns
    /// the index of its `run` in this instance's function table. (Calling it
    /// from here keeps JS frames off the stack, so that the thread can be
    /// suspended with JSPI, see launcher/src/jvm/thread.worker.ts.)
    fn instantiate(module: u32, wasm: *const u8, len: usize, method: u32, table_size: u32) -> u32;
    /// Grows this worker's table to include `index`; returns its size.
    fn table_ensure(index: u32) -> u32;
    /// This worker's native stack size in bytes (measured at its start).
    fn native_stack() -> u32;
    /// WebAssembly.validate of `wasm[..len]`.
    fn validate(wasm: *const u8, len: usize) -> bool;
}

thread_local! {
    /// The interpreter thread running compiled code on this worker.
    static THREAD: Cell<*mut ()> = const { Cell::new(std::ptr::null_mut()) };
    /// This worker's table size, as last reported by `table_ensure`.
    static TABLE_SIZE: Cell<u32> = const { Cell::new(0) };
    /// The modules' `run` functions instantiated in this worker, by module.
    static RUNS: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Default)]
pub struct WebJit {
    /// Compiled modules: bytes, method, table size.
    modules: RwLock<Vec<(&'static [u8], u32, u32)>>,
}

impl WebJit {
    pub fn new() -> WebJit {
        WebJit::default()
    }
}

impl JitBackend for WebJit {
    fn load(&self, wasm: &[u8], method_index: u32, table_size: u32) -> Result<u32, String> {
        // SAFETY: reads `wasm` only.
        if !unsafe { validate(wasm.as_ptr(), wasm.len()) } {
            return Err("module does not validate".into());
        }
        let bytes: &'static [u8] = Box::leak(wasm.to_vec().into_boxed_slice());
        let mut modules = self.modules.write().unwrap();
        modules.push((bytes, method_index, table_size));
        Ok(modules.len() as u32 - 1)
    }

    unsafe fn enter(&self, module: u32, thread: *mut (), fp: u32, record: u32, block: u32) -> i32 {
        let index = RUNS.with(|runs| runs.borrow().get(module as usize).copied().unwrap_or(0));
        let index = if index != 0 {
            index
        } else {
            let (bytes, method, table_size) = self.modules.read().unwrap()[module as usize];
            // SAFETY: the bytes are a valid module from `compile`.
            let index = unsafe { instantiate(module, bytes.as_ptr(), bytes.len(), method, table_size) };
            RUNS.with(|runs| {
                let mut runs = runs.borrow_mut();
                if runs.len() <= module as usize {
                    runs.resize(module as usize + 1, 0);
                }
                runs[module as usize] = index;
            });
            index
        };
        // SAFETY: a table index of a `run(fp, record, block) -> status`.
        let run: extern "C" fn(u32, u32, u32) -> i32 = unsafe { std::mem::transmute(index as usize) };
        let previous = THREAD.replace(thread);
        let result = run(fp, record, block);
        THREAD.set(previous);
        result
    }

    fn native_stack(&self) -> Option<u32> {
        // SAFETY: no arguments; answered by the JS side.
        Some(unsafe { native_stack() })
    }
}

fn thread() -> *mut () {
    THREAD.get()
}

// The compiled modules' imports (module "vm"), exported by rjava.wasm.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_step(record: u32, pc: u32, depth: u32) -> i32 {
    unsafe { jvm::jit::host_step(thread(), record, pc, depth) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_invoke(record: u32, pc: u32, depth: u32) -> i32 {
    unsafe { jvm::jit::host_invoke(thread(), record, pc, depth) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_select(record: u32, class_id: u32, method: u32) -> i32 {
    let index = unsafe { jvm::jit::host_select(thread(), record, class_id, method) };
    if index >= 0 && index as u32 >= TABLE_SIZE.get() {
        TABLE_SIZE.set(unsafe { table_ensure(index as u32) });
    }
    index
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_interpret(record: u32, pc: u32, depth: u32) -> i32 {
    unsafe { jvm::jit::host_interpret(thread(), record, pc, depth) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_safepoint(record: u32) {
    unsafe { jvm::jit::host_safepoint(thread(), record) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_overflow(record: u32) -> i32 {
    unsafe { jvm::jit::host_overflow(thread(), record) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_subtype(class: u32, target: u32) -> i32 {
    unsafe { jvm::jit::host_subtype(thread(), class, target) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_pure(op: u32, a: u32, b: u32) -> i32 {
    unsafe { jvm::jit::host_pure(thread(), op, a, b) }
}

#[unsafe(no_mangle)]
pub extern "C" fn rjava_jit_fmod(a: f64, b: f64) -> f64 {
    jvm::jit::host_fmod(a, b)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_monitor(record: u32, enter: u32) -> i32 {
    unsafe { jvm::jit::host_monitor(thread(), record, enter) }
}

/// The table's default entry: a method without compiled code in this worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rjava_jit_call(fp: u32, record: u32, locals: u32, method: u32) -> i32 {
    unsafe { jvm::jit::host_call(thread(), fp, record, locals, method) }
}

/// Called by the JS side when it grows the table on instantiating a module.
#[unsafe(no_mangle)]
pub extern "C" fn rjava_jit_table_size(size: u32) {
    TABLE_SIZE.set(size);
}
