//! Just-in-time compilation of hot methods to WebAssembly.
//!
//! The compiler (`compile`) is target-independent; a `JitBackend` turns the
//! modules into executable code: wasmtime natively, the WebAssembly JS API in
//! the browser. Compiled code works on the interpreter's frames in the VM's
//! linear memory, so the two can hand a frame back and forth at block
//! boundaries (see `compile` for the protocol).

pub mod compile;
mod locals;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// Executes compiled modules (see `compile` for what they import and export).
/// The backend forwards the imports to the `host_*` functions and keeps,
/// per thread, the `vm.table` of `call` functions by method index (the
/// `host_call` stub where none is loaded).
pub trait JitBackend: Send + Sync {
    /// Compiles the module of method `method_index`, which calls methods
    /// below `table_size` through the table; returns its handle.
    fn load(&self, wasm: &[u8], method_index: u32, table_size: u32) -> Result<u32, String>;

    /// Calls `run(fp, record, block)` of a loaded module on the current thread.
    ///
    /// # Safety
    /// `thread` must be the current thread's `interp::Thread`, which stays
    /// borrowed by the call (the imports use it).
    unsafe fn enter(&self, module: u32, thread: *mut (), fp: u32, record: u32, block: u32) -> i32;

    /// The method whose compiled code contains native address `pc` (for
    /// profiling), if the backend knows.
    fn method_at(&self, _pc: usize) -> Option<u32> {
        None
    }

    /// Bytes of native stack compiled code may use on the current thread, if
    /// limited (the browser's engine stacks are small; wasmtime's is not).
    fn native_stack(&self) -> Option<u32> {
        None
    }
}

/// Native stack charged for leaving compiled code to the interpreter and
/// entering compiled code again (the VM's own frames in between).
pub const REENTRY_COST: i32 = 8 << 10;
/// Native stack kept back for the VM's own deep paths (class loading,
/// natives, collection) below the last budget check.
pub const NATIVE_RESERVE: u32 = 128 << 10;

/// A method's compiled code.
pub struct JitCode {
    pub module: u32,
    /// Block index for each pc where compiled code can be entered.
    pub entry_block: Box<[u16]>,
}

impl JitCode {
    #[inline]
    pub fn block_at(&self, pc: u32) -> Option<u32> {
        match self.entry_block.get(pc as usize) {
            Some(&block) if block != compile::NO_BLOCK => Some(block as u32),
            _ => None,
        }
    }
}

/// Per-method JIT state.
#[derive(Default)]
pub struct MethodJit {
    /// Set once: the compiled code, or None if compilation failed.
    pub code: OnceLock<Option<JitCode>>,
    /// The newest of up to MAX_RECOMPILES versions compiled again later,
    /// when call sites that had to go through `invoke` (their targets not
    /// yet loaded, initialised or linked) can be direct calls (see
    /// `Thread::jit_invoke`). Leaked: older versions may still be running.
    pub recompiled: std::sync::atomic::AtomicPtr<JitCode>,
    /// How many times the method was compiled again: under wasm, each
    /// version's `call` entry checks it is still the newest and else leaves
    /// to the interpreter, which enters (and so installs) the newest.
    pub version: AtomicU32,
    pub recompile_queued: std::sync::atomic::AtomicBool,
    /// Calls this method's compiled code made through `invoke`.
    pub generic_calls: AtomicU32,
    /// Calls into the baseline version (counted by its `call` entry, under
    /// wasm); at `tier2_threshold` the method is compiled again as tier 2.
    pub calls: AtomicU32,
    /// 2 once the method was found hot: later compilations use the tier 2
    /// settings (larger inlining budget).
    pub tier: std::sync::atomic::AtomicU8,
    /// Backward branches taken in the interpreter (loop hotness).
    pub backedges: AtomicU32,
    /// Submitted to the compiler threads.
    pub queued: std::sync::atomic::AtomicBool,
}

impl MethodJit {
    /// The newest compiled code, if any.
    #[inline]
    pub fn current(&self) -> Option<&JitCode> {
        // SAFETY: published once compiled, never freed.
        match unsafe { self.recompiled.load(Ordering::Acquire).as_ref() } {
            Some(code) => Some(code),
            None => self.code.get()?.as_ref(),
        }
    }

    /// The version a compilation starting now produces.
    pub fn next_version(&self) -> u32 {
        self.version.load(Ordering::Acquire) + self.code.get().is_some() as u32
    }

    /// Counts a backward branch (submitting the method for compilation when
    /// its loops are hot); true if compiled code can take over.
    #[inline]
    pub fn on_backedge(&self, vm: &crate::vm::Vm, method: crate::class::MethodRef) -> bool {
        match self.code.get() {
            Some(code) => code.is_some(),
            None => {
                let count = self.backedges.load(Ordering::Relaxed) + 1;
                self.backedges.store(count, Ordering::Relaxed);
                if count >= thresholds().backedges {
                    vm.jit_queue.submit(vm.as_static(), method);
                }
                false
            }
        }
    }
}

/// Times a method may be compiled again (see `MethodJit::recompiled`).
pub const MAX_RECOMPILES: u32 = 3;

/// Methods waiting to be compiled, and the compiler threads that take them
/// (RJAVA_JIT_THREADS; Java threads keep interpreting meanwhile). With
/// RJAVA_JIT_SYNC, the submitting thread compiles right away instead.
#[derive(Default)]
pub struct Queue {
    pending: std::sync::Mutex<std::collections::VecDeque<crate::class::MethodRef>>,
    cond: std::sync::Condvar,
    started: OnceLock<()>,
}

impl Queue {
    /// Compiles `method` (already compiled) once more (not with
    /// RJAVA_NO_RECOMPILE).
    pub fn resubmit(&self, vm: &'static crate::vm::Vm, method: crate::class::MethodRef) {
        static OFF: OnceLock<bool> = OnceLock::new();
        if *OFF.get_or_init(|| std::env::var_os("RJAVA_NO_RECOMPILE").is_some()) {
            return;
        }
        if method.jit.code.get().is_none()
            || method.jit.version.load(Ordering::Relaxed) >= MAX_RECOMPILES
            || method.jit.recompile_queued.swap(true, Ordering::Relaxed)
        {
            return;
        }
        self.pending.lock().unwrap().push_back(method);
        self.cond.notify_one();
        let _ = vm;
    }

    pub fn submit(&self, vm: &'static crate::vm::Vm, method: crate::class::MethodRef) {
        if method.jit.queued.swap(true, Ordering::Relaxed) {
            return;
        }
        static SYNC: OnceLock<bool> = OnceLock::new();
        if *SYNC.get_or_init(|| std::env::var_os("RJAVA_JIT_SYNC").is_some()) {
            let _ = method.jit.code.set(crate::interp::compile_method(vm, method));
            return;
        }
        self.pending.lock().unwrap().push_back(method);
        self.cond.notify_one();
        self.started.get_or_init(|| {
            let threads = std::env::var("RJAVA_JIT_THREADS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| (crate::processors() / 3).clamp(1, 4));
            for i in 0..threads {
                std::thread::Builder::new()
                    .name(format!("JIT compiler {i}"))
                    // Under wasm the stack has no guard page: be generous.
                    .stack_size(16 << 20)
                    .spawn(move || vm.jit_queue.work(vm))
                    .expect("spawning a JIT compiler thread");
            }
        });
    }

    fn work(&self, vm: &'static crate::vm::Vm) {
        loop {
            let method = {
                let mut pending = self.pending.lock().unwrap();
                loop {
                    match pending.pop_front() {
                        Some(method) => break method,
                        None => pending = self.cond.wait(pending).unwrap(),
                    }
                }
            };
            if method.jit.code.get().is_some() {
                let version = method.jit.next_version();
                if let Some(code) = crate::interp::compile_method(vm, method) {
                    method.jit.recompiled.store(Box::leak(Box::new(code)), Ordering::Release);
                    method.jit.version.store(version, Ordering::Release);
                }
                method.jit.recompile_queued.store(false, Ordering::Relaxed);
            } else {
                let _ = method.jit.code.set(crate::interp::compile_method(vm, method));
            }
        }
    }
}

pub struct Thresholds {
    pub invocations: u64,
    pub backedges: u32,
}

/// RJAVA_JIT_THRESHOLD=<invocations> (back edges: ×10).
pub fn thresholds() -> &'static Thresholds {
    static THRESHOLDS: OnceLock<Thresholds> = OnceLock::new();
    THRESHOLDS.get_or_init(|| {
        let invocations = std::env::var("RJAVA_JIT_THRESHOLD").ok().and_then(|v| v.parse().ok()).unwrap_or(1000);
        Thresholds { invocations, backedges: (invocations as u32).saturating_mul(10) }
    })
}

/// RJAVA_JIT_LOG: report compilations.
pub fn log_enabled() -> bool {
    static LOG: OnceLock<bool> = OnceLock::new();
    *LOG.get_or_init(|| std::env::var_os("RJAVA_JIT_LOG").is_some())
}

fn thread<'a>(thread: *mut ()) -> &'a mut crate::interp::Thread<'static> {
    // SAFETY: the pointer passed to `JitBackend::enter` (see the callers).
    unsafe { &mut *(thread as *mut crate::interp::Thread<'static>) }
}

// The imports. Safety, for all: `thread` is the pointer passed to the
// `JitBackend::enter` call running the importing code.

/// `vm.step(record, pc, depth) -> status`: the interpreter executes the
/// (non-call) instruction at `pc` of the frame at `record`, whose operand
/// stack has `depth` slots.
///
/// # Safety
/// See above.
pub unsafe fn host_step(t: *mut (), record: u32, pc: u32, depth: u32) -> i32 {
    thread(t).jit_step(record, pc, depth)
}

/// `vm.invoke(record, pc, depth) -> status`: executes the call at `pc`.
///
/// # Safety
/// See above.
pub unsafe fn host_invoke(t: *mut (), record: u32, pc: u32, depth: u32) -> i32 {
    thread(t).jit_invoke(record, pc, depth)
}

/// `vm.select(record, class_id, method) -> method index or -1`: dispatch
/// for the inline caches and the dispatch cache.
///
/// # Safety
/// See above.
pub unsafe fn host_select(t: *mut (), record: u32, class_id: u32, method: u32) -> i32 {
    thread(t).jit_select(record, class_id, method)
}

/// `vm.interpret(record, pc, depth) -> status`: the interpreter runs the rest
/// of the frame.
///
/// # Safety
/// See above.
pub unsafe fn host_interpret(t: *mut (), record: u32, pc: u32, depth: u32) -> i32 {
    thread(t).jit_interpret(record, pc, depth)
}

/// `vm.safepoint(record)`.
///
/// # Safety
/// See above.
pub unsafe fn host_safepoint(t: *mut (), record: u32) {
    thread(t).jit_safepoint(record)
}

/// Operations of the `vm.pure(op, a, b)` import: natives that need neither
/// allocation nor a safepoint. -1: not answered (the caller then makes the
/// call through `invoke`).
pub mod pure {
    pub const CURRENT_THREAD: u32 = 0;
    /// System.identityHashCode(a)
    pub const IDENTITY_HASH: u32 = 1;
    /// a.getClass()
    pub const GET_CLASS: u32 = 2;
    /// a.hashCode() if a's class does not override Object.hashCode
    pub const OBJECT_HASH: u32 = 3;
    /// a.isInstance(b), a a Class
    pub const IS_INSTANCE: u32 = 4;
    /// Unsafe a: a.allocateInstance(b), from the thread's allocation buffer
    /// (b initialised and concrete; no collection).
    pub const ALLOCATE_INSTANCE: u32 = 5;
    /// aastore check: may b be stored in array a (both non-null)?
    pub const ARRAY_STORE_CHECK: u32 = 6;
    /// Method a (a global index) became hot: compile it as tier 2.
    pub const TIER_UP: u32 = 7;
}

/// Calls of a method's baseline code before it is compiled as tier 2
/// (RJAVA_JIT_TIER2=<calls>, e.g. 20000; 0, the default: no tier 2, which
/// measured no faster on the integrated server).
pub fn tier2_threshold() -> u32 {
    static THRESHOLD: OnceLock<u32> = OnceLock::new();
    *THRESHOLD.get_or_init(|| std::env::var("RJAVA_JIT_TIER2").ok().and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// `vm.pure(op, a, b) -> result or -1`.
///
/// # Safety
/// See above.
pub unsafe fn host_pure(t: *mut (), op: u32, a: u32, b: u32) -> i32 {
    use crate::heap::Ref;
    let t = thread(t);
    let vm = t.vm;
    let (a, b) = (Ref(a), Ref(b));
    match op {
        pure::CURRENT_THREAD => t.java_thread.0 as i32,
        pure::IDENTITY_HASH => if a.is_null() { 0 } else { vm.identity_hash(a) },
        pure::GET_CLASS if !a.is_null() => {
            let mirror = vm.class_of(a).mirror.get();
            if mirror.is_null() { -1 } else { mirror.0 as i32 }
        }
        pure::OBJECT_HASH if !a.is_null() => {
            // Object.hashCode and its vtable slot.
            static OBJECT_HASH_CODE: std::sync::OnceLock<Option<(usize, usize)>> = std::sync::OnceLock::new();
            let object_hash = OBJECT_HASH_CODE.get_or_init(|| {
                let m = vm.find_loaded("java/lang/Object")?.find_method("hashCode", "()I")?;
                Some((m as *const crate::class::Method as usize, m.vtable_index? as usize))
            });
            match *object_hash {
                Some((method, slot))
                    if vm.class_of(a).vtable.get(slot).is_some_and(|&m| m as *const crate::class::Method as usize == method) =>
                {
                    vm.identity_hash(a)
                }
                _ => -1,
            }
        }
        pure::ALLOCATE_INSTANCE if !b.is_null() => {
            let class = vm.class_from_mirror(b);
            let concrete = class.kind.is_instance() && !class.is_interface() && class.access_flags & classfile::access::ABSTRACT == 0;
            if !concrete || !class.initialized.load(std::sync::atomic::Ordering::Acquire) {
                return -1;
            }
            match crate::threads::tlab_alloc(&vm.heap, class.instance_size.max(8).next_multiple_of(8)) {
                Some(obj) => {
                    vm.heap.write(obj, 0, class.id);
                    obj.0 as i32
                }
                None => -1,
            }
        }
        pure::ARRAY_STORE_CHECK if !a.is_null() && !b.is_null() => match vm.class_of(a).kind {
            crate::class::ClassKind::Array { component } => vm.class_of(b).is_subclass_of(component) as i32,
            _ => 0,
        },
        pure::TIER_UP => {
            if let Some(method) = vm.method_if_any(a.0) {
                method.jit.tier.store(2, std::sync::atomic::Ordering::Relaxed);
                vm.jit_queue.resubmit(vm.as_static(), method);
            }
            0
        }
        pure::IS_INSTANCE if !a.is_null() => (!b.is_null() && vm.class_of(b).is_subclass_of(vm.class_from_mirror(a))) as i32,
        _ => -1,
    }
}

/// `vm.monitor(record, enter) -> status`: enters (1) or exits (0) the
/// monitor of the synchronized method whose frame record is at `record`.
///
/// # Safety
/// See above.
pub unsafe fn host_monitor(t: *mut (), record: u32, enter: u32) -> i32 {
    thread(t).jit_monitor(record, enter != 0)
}

/// `vm.fmod(a, b)`: Java's floating-point remainder (C fmod).
pub fn host_fmod(a: f64, b: f64) -> f64 {
    a % b
}

/// `vm.subtype(class id, target class id) -> bool`: whether instances of the
/// first class are assignable to the second (checkcast, instanceof).
///
/// # Safety
/// See above.
pub unsafe fn host_subtype(t: *mut (), class: u32, target: u32) -> i32 {
    let vm = thread(t).vm;
    vm.class_by_id(class).is_subclass_of(vm.class_by_id(target)) as i32
}

/// `vm.overflow(record) -> status`: throws StackOverflowError.
///
/// # Safety
/// See above.
pub unsafe fn host_overflow(t: *mut (), record: u32) -> i32 {
    thread(t).jit_overflow(record)
}

/// The table stub, `(fp, record, locals, method) -> status`: calls a method
/// that has no `call` function in the table (yet).
///
/// # Safety
/// See above.
pub unsafe fn host_call(t: *mut (), _fp: u32, record: u32, locals: u32, method: u32) -> i32 {
    thread(t).jit_call(record, locals, method)
}

/// RJAVA_JIT_STATS: counters of how compiled code hands work to the VM,
/// printed (as deltas) every 10 seconds.
pub mod stats {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    use std::sync::{Mutex, OnceLock};

    pub struct Stats {
        pub compiled: AtomicU64,
        pub compile_us: AtomicU64,
        pub failed: Mutex<std::collections::HashMap<String, u64>>,
        /// `step` imports by opcode.
        pub step: [AtomicU64; 256],
        /// `interpret` imports (slow paths) by opcode.
        pub interpret: [AtomicU64; 256],
        pub invoke: AtomicU64,
        /// Why calls went through `invoke`, with an example method.
        pub invoke_reasons: Mutex<std::collections::HashMap<&'static str, (u64, String)>>,
        /// Calls through `invoke` by reason and target method (its address).
        pub invoke_targets: Mutex<std::collections::HashMap<(&'static str, usize, usize), (u64, crate::class::MethodRef, crate::class::MethodRef)>>,
        pub select: AtomicU64,
        pub call_stub: AtomicU64,
        /// Frames run by the interpreter (pushed and not entered compiled).
        pub interpreted_frames: AtomicU64,
        /// Frames of methods without compiled code, by method (and whether
        /// compiling it failed).
        pub uncompiled: Mutex<std::collections::HashMap<usize, (u64, crate::class::MethodRef, bool)>>,
        pub compiled_entries: AtomicU64,
        /// Virtual call sites in compiled code (wasm), with counters that
        /// the code increments.
        pub sites: Mutex<Vec<Site>>,
    }

    /// A virtual call site: an inlined target behind a class guard
    /// (counters: executions, guard misses), or a call through the site's
    /// inline cache (counters: calls, cache misses).
    pub struct Site {
        pub counters: &'static [std::sync::atomic::AtomicU32; 2],
        pub guarded: bool,
        pub caller: crate::class::MethodRef,
        pub pc: u32,
        pub target: crate::class::MethodRef,
    }

    static ENABLED: AtomicBool = AtomicBool::new(false);

    /// Calls into compiled `call` entries (counted by compiled code under
    /// wasm while statistics are on).
    pub static COMPILED_CALLS: AtomicU64 = AtomicU64::new(0);

    /// Whether statistics are on (without starting the reporter).
    pub fn enabled() -> bool {
        std::env::var_os("RJAVA_JIT_STATS").is_some()
    }

    #[inline]
    pub fn get() -> Option<&'static Stats> {
        if !ENABLED.load(Relaxed) {
            return None;
        }
        Some(stats())
    }

    fn stats() -> &'static Stats {
        static STATS: OnceLock<Stats> = OnceLock::new();
        STATS.get_or_init(|| Stats {
            compiled: AtomicU64::new(0),
            compile_us: AtomicU64::new(0),
            failed: Mutex::new(Default::default()),
            step: std::array::from_fn(|_| AtomicU64::new(0)),
            interpret: std::array::from_fn(|_| AtomicU64::new(0)),
            invoke: AtomicU64::new(0),
            invoke_reasons: Mutex::new(Default::default()),
            invoke_targets: Mutex::new(Default::default()),
            select: AtomicU64::new(0),
            call_stub: AtomicU64::new(0),
            interpreted_frames: AtomicU64::new(0),
            uncompiled: Mutex::new(Default::default()),
            compiled_entries: AtomicU64::new(0),
            sites: Mutex::new(Vec::new()),
        })
    }

    #[inline]
    pub fn bump(counter: impl FnOnce(&Stats) -> &AtomicU64) {
        if let Some(stats) = get() {
            counter(stats).fetch_add(1, Relaxed);
        }
    }

    /// Starts the reporter if RJAVA_JIT_STATS is set.
    pub fn init() {
        if std::env::var_os("RJAVA_JIT_STATS").is_none() || ENABLED.swap(true, Relaxed) {
            return;
        }
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            report();
        });
    }

    fn take(counter: &AtomicU64) -> u64 {
        counter.swap(0, Relaxed)
    }

    /// The virtual call sites that missed most (summed over the compiled
    /// copies of each site).
    fn report_sites(s: &Stats) -> String {
        let mut sums: std::collections::HashMap<(usize, u32, bool), (u64, u64, &Site)> = Default::default();
        let sites = s.sites.lock().unwrap();
        for site in sites.iter() {
            let (n, misses) = (site.counters[0].swap(0, Relaxed) as u64, site.counters[1].swap(0, Relaxed) as u64);
            let entry = sums.entry((&*site.caller as *const _ as usize, site.pc, site.guarded)).or_insert((0, 0, site));
            entry.0 += n;
            entry.1 += misses;
        }
        let mut out = String::new();
        for (guarded, title) in [(true, "guard misses (inlined target, other class)"), (false, "inline cache misses (calls)")] {
            let mut top: Vec<_> = sums.iter().filter(|(k, v)| k.2 == guarded && v.1 > 0).map(|(_, v)| *v).collect();
            top.sort_by(|a, b| b.1.cmp(&a.1));
            let total: u64 = top.iter().map(|v| v.1).sum();
            if total == 0 {
                continue;
            }
            out += &format!("\n    {title}: {total}");
            for (n, misses, site) in top.iter().take(20) {
                out += &format!("\n      {misses:>9}/{n:<9} {:?} @{} -> {:?}", site.caller, site.pc, site.target);
            }
        }
        out
    }

    fn report() {
        let s = stats();
        let compiled = take(&s.compiled);
        let mut out = format!(
            "[jit-stats] compiled {compiled} ({} ms) | compiled calls {} | interpreted frames {} | compiled entries {} | invoke {} select {} stub {}",
            take(&s.compile_us) / 1000,
            take(&COMPILED_CALLS),
            take(&s.interpreted_frames),
            take(&s.compiled_entries),
            take(&s.invoke),
            take(&s.select),
            take(&s.call_stub),
        );
        for (name, table) in [("step", &s.step), ("interpret", &s.interpret)] {
            let mut top: Vec<(u64, usize)> = table.iter().enumerate().map(|(op, c)| (take(c), op)).filter(|(c, _)| *c > 0).collect();
            top.sort_unstable_by(|a, b| b.cmp(a));
            if !top.is_empty() {
                out += &format!("\n    {name}:");
                for (count, op) in top.iter().take(10) {
                    out += &format!(" {}={count}", classfile::opcodes::name(*op as u8));
                }
            }
        }
        let mut reasons: Vec<(&str, (u64, String))> = s.invoke_reasons.lock().unwrap().drain().collect();
        if !reasons.is_empty() {
            reasons.sort_by(|a, b| b.1.0.cmp(&a.1.0));
            out += "\n    invoke:";
            for (reason, (count, example)) in reasons.iter().take(8) {
                out += &format!(" {reason}={count} (e.g. {example})");
            }
        }
        let mut targets: Vec<_> = s.invoke_targets.lock().unwrap().drain().collect();
        if !targets.is_empty() {
            targets.sort_by(|a, b| b.1.0.cmp(&a.1.0));
            out += "\n    invoke targets:";
            for ((reason, _, _), (count, method, caller)) in targets.iter().take(25) {
                out += &format!("\n      {count:>9} {reason} {method:?} from {caller:?}");
            }
        }
        let mut uncompiled: Vec<_> = s.uncompiled.lock().unwrap().drain().map(|(_, v)| v).collect();
        if !uncompiled.is_empty() {
            uncompiled.sort_by(|a, b| b.0.cmp(&a.0));
            out += "\n    interpreted (no compiled code):";
            for (count, method, failed) in uncompiled.iter().take(15) {
                out += &format!("\n      {count:>9} {method:?}{}", if *failed { " [compile failed]" } else { "" });
            }
        }
        out += &report_sites(s);
        let mut failed: Vec<(String, u64)> = s.failed.lock().unwrap().drain().collect();
        if !failed.is_empty() {
            failed.sort_by(|a, b| b.1.cmp(&a.1));
            out += "\n    not compiled:";
            for (reason, count) in failed.iter().take(8) {
                out += &format!(" {reason}={count}");
            }
        }
        eprintln!("{out}");
    }
}
