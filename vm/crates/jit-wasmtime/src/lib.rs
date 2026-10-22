//! Native JIT backend: runs the modules produced by `jvm::jit::compile` with
//! wasmtime (Cranelift).
//!
//! The VM's linear memory is a wasmtime shared memory, imported by every
//! module. Stores are per OS thread (each Java thread is one), each with its
//! own `vm.table`; a module is instantiated lazily in a thread's store the
//! first time that thread runs or calls it. Imports may re-enter compiled
//! code (e.g. `step` running a static initialiser), which then runs in the
//! store context of the import's caller.

use std::cell::{Cell, RefCell, UnsafeCell};
use std::sync::RwLock;

use jvm::jit::JitBackend;
use jvm::memory::{LinearMemory, Region};
use wasmtime::{
    AsContextMut, Caller, Config, Engine, Extern, Func, Instance, MemoryType, Module, OptLevel, Ref, RefType,
    SharedMemory, Store, StoreContextMut, Table, TableType, TypedFunc,
};

/// Store data: the interpreter thread running compiled code.
struct Data {
    thread: *mut (),
}

type RunFn = TypedFunc<(i32, i32, i32), i32>;

/// Native stack that compiled code may use (direct calls nest natively).
pub const MAX_WASM_STACK: usize = 48 << 20;

pub struct WasmtimeJit {
    engine: Engine,
    memory: SharedMemory,
    /// Compiled modules, the method each belongs to and the table size it needs.
    modules: RwLock<Vec<(Module, u32, u32)>>,
    /// Code address ranges of the modules and their methods, for profiling.
    code_ranges: RwLock<Vec<(std::ops::Range<usize>, u32)>>,
}

struct PerThread {
    store: UnsafeCell<Store<Data>>,
    /// The imports, in the order the modules import them.
    imports: Vec<Extern>,
    table: Table,
    stub: Func,
    /// Instantiated `run` functions by module handle.
    funcs: RefCell<Vec<Option<&'static RunFn>>>,
}

thread_local! {
    static THREAD: RefCell<Option<&'static PerThread>> = const { RefCell::new(None) };
    /// The innermost import's `Caller` while an import runs (a
    /// `*mut Caller<'_, Data>`), else null.
    static CALLER: Cell<*mut ()> = const { Cell::new(std::ptr::null_mut()) };
}

impl WasmtimeJit {
    /// A backend with a linear memory of `size` bytes (a multiple of 64 KiB).
    pub fn new(size: usize) -> Result<WasmtimeJit, String> {
        let mut config = Config::new();
        config
            .wasm_threads(true)
            .shared_memory(true)
            .cranelift_opt_level(OptLevel::Speed)
            .max_wasm_stack(MAX_WASM_STACK)
            .async_stack_size(MAX_WASM_STACK + (1 << 20)); // unused, but must be ≥ max_wasm_stack
        let engine = Engine::new(&config).map_err(|e| e.to_string())?;
        let pages = (size / 65536) as u32;
        let memory = SharedMemory::new(&engine, MemoryType::shared(pages, pages)).map_err(|e| e.to_string())?;
        Ok(WasmtimeJit { engine, memory, modules: RwLock::new(Vec::new()), code_ranges: RwLock::new(Vec::new()) })
    }

    /// The memory region, for `VmOptions::memory`.
    pub fn region(&self) -> Region {
        let data = self.memory.data();
        Region { base: data.as_ptr() as *mut u8, size: data.len() }
    }

    /// Memory size for a heap and thread count, rounded up to whole pages.
    pub fn memory_size(heap_size: usize, threads: usize) -> usize {
        LinearMemory::required_size(heap_size, threads).next_multiple_of(65536)
    }

    fn per_thread(&self) -> &'static PerThread {
        THREAD.with(|cell| *cell.borrow_mut().get_or_insert_with(|| self.new_per_thread()))
    }

    fn new_per_thread(&self) -> &'static PerThread {
        let mut store = Store::new(&self.engine, Data { thread: std::ptr::null_mut() });
        let stub = Func::wrap(
            &mut store,
            |mut caller: Caller<'_, Data>, fp: i32, record: i32, locals: i32, method: i32| -> i32 {
                let t = caller.data().thread;
                with_caller(&mut caller, || unsafe {
                    jvm::jit::host_call(t, fp as u32, record as u32, locals as u32, method as u32)
                })
            },
        );
        let table = Table::new(&mut store, TableType::new(RefType::FUNCREF, 0, None), Ref::Func(Some(stub)))
            .expect("JIT table");
        macro_rules! import3 {
            ($host:path) => {
                Func::wrap(&mut store, |mut caller: Caller<'_, Data>, a: i32, b: i32, c: i32| -> i32 {
                    let t = caller.data().thread;
                    with_caller(&mut caller, || unsafe { $host(t, a as u32, b as u32, c as u32) })
                })
            };
        }
        let step = import3!(jvm::jit::host_step);
        let invoke = import3!(jvm::jit::host_invoke);
        let select = Func::wrap(&mut store, |mut caller: Caller<'_, Data>, record: i32, class: i32, method: i32| -> i32 {
            let t = caller.data().thread;
            let index = with_caller(&mut caller, || unsafe {
                jvm::jit::host_select(t, record as u32, class as u32, method as u32)
            });
            if index >= 0 {
                let table = THREAD.with(|cell| cell.borrow().unwrap().table);
                ensure_table_size(&mut caller, table, index as u32);
            }
            index
        });
        let interpret = import3!(jvm::jit::host_interpret);
        let safepoint = Func::wrap(&mut store, |mut caller: Caller<'_, Data>, record: i32| {
            let t = caller.data().thread;
            with_caller(&mut caller, || unsafe { jvm::jit::host_safepoint(t, record as u32) })
        });
        let overflow = Func::wrap(&mut store, |mut caller: Caller<'_, Data>, record: i32| -> i32 {
            let t = caller.data().thread;
            with_caller(&mut caller, || unsafe { jvm::jit::host_overflow(t, record as u32) })
        });
        let subtype = Func::wrap(&mut store, |caller: Caller<'_, Data>, class: i32, target: i32| -> i32 {
            unsafe { jvm::jit::host_subtype(caller.data().thread, class as u32, target as u32) }
        });
        let pure = Func::wrap(&mut store, |caller: Caller<'_, Data>, op: i32, a: i32, b: i32| -> i32 {
            unsafe { jvm::jit::host_pure(caller.data().thread, op as u32, a as u32, b as u32) }
        });
        let fmod = Func::wrap(&mut store, |a: f64, b: f64| -> f64 { jvm::jit::host_fmod(a, b) });
        let monitor = Func::wrap(&mut store, |mut caller: Caller<'_, Data>, record: i32, enter: i32| -> i32 {
            let t = caller.data().thread;
            with_caller(&mut caller, || unsafe { jvm::jit::host_monitor(t, record as u32, enter as u32) })
        });
        let imports = vec![
            Extern::SharedMemory(self.memory.clone()),
            Extern::Table(table),
            Extern::Func(step),
            Extern::Func(invoke),
            Extern::Func(select),
            Extern::Func(interpret),
            Extern::Func(safepoint),
            Extern::Func(overflow),
            Extern::Func(stub),
            Extern::Func(subtype),
            Extern::Func(pure),
            Extern::Func(fmod),
            Extern::Func(monitor),
        ];
        // Lives as long as the thread; leaked to hand out 'static references.
        Box::leak(Box::new(PerThread {
            store: UnsafeCell::new(store),
            imports,
            table,
            stub,
            funcs: RefCell::new(Vec::new()),
        }))
    }

    /// The module's `run` in this thread's store, instantiating it (and
    /// entering its `call` in the table) on first use.
    fn func(&self, ctx: &mut StoreContextMut<'_, Data>, per_thread: &PerThread, module: u32) -> &'static RunFn {
        if let Some(Some(func)) = per_thread.funcs.borrow().get(module as usize) {
            return func;
        }
        let (wasm, method, table_size) = self.modules.read().unwrap()[module as usize].clone();
        ensure_table_size(&mut *ctx, per_thread.table, table_size - 1);
        let instance = Instance::new(&mut *ctx, &wasm, &per_thread.imports)
            .unwrap_or_else(|e| panic!("instantiating compiled code: {e:?}"));
        let func: &'static RunFn =
            Box::leak(Box::new(instance.get_typed_func(&mut *ctx, "run").expect("compiled module exports run")));
        let call = instance.get_func(&mut *ctx, "call").expect("compiled module exports call");
        ensure_table_size(&mut *ctx, per_thread.table, method);
        per_thread.table.set(&mut *ctx, method as u64, Ref::Func(Some(call))).expect("table set");
        let mut funcs = per_thread.funcs.borrow_mut();
        if funcs.len() <= module as usize {
            funcs.resize(module as usize + 1, None);
        }
        funcs[module as usize] = Some(func);
        func
    }
}

/// Grows `table` (with the stub) to include `index`.
fn ensure_table_size(mut ctx: impl AsContextMut<Data = Data>, table: Table, index: u32) {
    let size = table.size(&ctx);
    if size <= index as u64 {
        let stub = THREAD.with(|cell| cell.borrow().unwrap().stub);
        let wanted = (index as u64 + 1).max(size * 2).max(1024);
        table.grow(&mut ctx, wanted - size, Ref::Func(Some(stub))).expect("table grow");
    }
}

/// Runs `f` with `caller` as the context for nested calls into compiled code.
fn with_caller<R>(caller: &mut Caller<'_, Data>, f: impl FnOnce() -> R) -> R {
    let previous = CALLER.with(|c| c.replace(caller as *mut Caller<'_, Data> as *mut ()));
    let result = f();
    CALLER.with(|c| c.set(previous));
    result
}

impl JitBackend for WasmtimeJit {
    fn load(&self, wasm: &[u8], method_index: u32, table_size: u32) -> Result<u32, String> {
        let module = Module::new(&self.engine, wasm).map_err(|e| format!("{e:?}"))?;
        let range = module.image_range();
        self.code_ranges.write().unwrap().push((range.start as usize..range.end as usize, method_index));
        let mut modules = self.modules.write().unwrap();
        modules.push((module, method_index, table_size));
        Ok(modules.len() as u32 - 1)
    }

    fn method_at(&self, pc: usize) -> Option<u32> {
        self.code_ranges.read().unwrap().iter().find(|(range, _)| range.contains(&pc)).map(|&(_, method)| method)
    }

    unsafe fn enter(&self, module: u32, thread: *mut (), fp: u32, record: u32, block: u32) -> i32 {
        let per_thread = self.per_thread();
        let caller = CALLER.with(|c| c.get());
        let mut ctx: StoreContextMut<'_, Data> = if caller.is_null() {
            // SAFETY: no compiled code is running on this thread, so the
            // store is not in use.
            unsafe { (*per_thread.store.get()).as_context_mut() }
        } else {
            // SAFETY: set by an import that is running on this thread.
            unsafe { (*(caller as *mut Caller<'static, Data>)).as_context_mut() }
        };
        let func = self.func(&mut ctx, per_thread, module);
        let previous = std::mem::replace(&mut ctx.data_mut().thread, thread);
        // Imports below this call use their own Caller; clear the outer one.
        let outer = CALLER.with(|c| c.replace(std::ptr::null_mut()));
        let result = func.call(&mut ctx, (fp as i32, record as i32, block as i32));
        CALLER.with(|c| c.set(outer));
        ctx.data_mut().thread = previous;
        result.unwrap_or_else(|trap| panic!("compiled code trapped: {trap:?}"))
    }
}
