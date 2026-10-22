//! Java threads, monitors and parking.
//!
//! Every Java thread runs on its own OS thread (a Web Worker in the browser)
//! with its own interpreter stack, in parallel with the others. The
//! collector stops the world: it raises the stop flag (in linear memory, so
//! compiled code polls it too) and waits until every other thread is parked
//! at a safepoint or outside Java code (in a blocking operation); those
//! threads wait to resume until the collection is over.

use crate::heap::{Ref, RefCell32};
use crate::interp::{JResult, Thread};
use std::cell::{Cell, UnsafeCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Which threads run Java code, and stop-the-world requests.
pub struct Safepoints {
    /// (threads running Java code, a collector is stopping the world)
    state: Mutex<(usize, bool)>,
    cond: Condvar,
    /// Non-zero while a collector wants the world stopped; polled at
    /// safepoints (by the interpreter and by compiled code).
    stop: &'static AtomicU32,
}

impl Safepoints {
    pub fn new(stop: &'static AtomicU32) -> Safepoints {
        Safepoints { state: Mutex::new((0, false)), cond: Condvar::new(), stop }
    }

    /// The calling thread starts (or resumes) running Java code; waits while
    /// the world is stopped.
    pub fn enter(&self) {
        let mut state = self.state.lock().unwrap();
        while state.1 {
            state = self.cond.wait(state).unwrap();
        }
        state.0 += 1;
    }

    /// The calling thread stops running Java code (blocking or exiting): it
    /// must not touch the heap until `enter`.
    pub fn leave(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 -= 1;
        self.cond.notify_all();
    }

    #[inline]
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed) != 0
    }

    /// At a safepoint: parks while a collector has the world stopped.
    #[inline]
    pub fn poll(&self) {
        if self.stopping() {
            self.leave();
            self.enter();
        }
    }

    /// Stops every other thread running Java code. False if another thread
    /// was collecting (this one waited for it; the caller should re-check
    /// whether it still needs to collect).
    pub fn stop_the_world(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.1 {
            state.0 -= 1;
            self.cond.notify_all();
            while state.1 {
                state = self.cond.wait(state).unwrap();
            }
            state.0 += 1;
            return false;
        }
        state.1 = true;
        self.stop.store(1, Ordering::SeqCst);
        while state.0 > 1 {
            state = self.cond.wait(state).unwrap();
        }
        true
    }

    pub fn resume(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = false;
        self.stop.store(0, Ordering::SeqCst);
        self.cond.notify_all();
    }
}

/// Runs `f`, which may block for long (waiting for a lock another thread can
/// hold across a collection), outside Java code if this is a Java thread.
pub(crate) fn blocking_region<R>(safepoints: &Safepoints, f: impl FnOnce() -> R) -> R {
    if !on_vm_thread() {
        return f();
    }
    safepoints.leave();
    let result = f();
    safepoints.enter();
    result
}

/// The interpreter (`interp::Thread`) attached on this OS thread, or null.
#[cfg(unix)]
pub(crate) fn current_interpreter() -> *mut () {
    // SAFETY: CURRENT points at the live ThreadShared of this thread.
    CURRENT.with(|c| unsafe { c.get().as_ref() }.map_or(std::ptr::null_mut(), |t| t.interpreter.load(Ordering::Acquire)))
}

/// The id of the Java thread on this OS thread (0 if none).
pub(crate) fn current_id() -> u64 {
    // SAFETY: CURRENT points at the live ThreadShared of this thread.
    CURRENT.with(|c| unsafe { c.get().as_ref() }.map_or(0, |t| t.id))
}

/// Per-thread state other threads interact with (unpark, interrupt, notify,
/// and the collector).
pub struct ThreadShared {
    pub id: u64,
    pub java_thread: RefCell32,
    wake: Mutex<WakeState>,
    cond: Condvar,
    /// References held by Rust code on this thread (see `push_handle`). Only
    /// the owning thread mutates it, and the collector reads it while that
    /// thread waits for the GIL.
    handles: UnsafeCell<Vec<Ref>>,
    /// The thread's interpreter (`Thread`), while attached, for stack scanning.
    interpreter: AtomicPtr<()>,
    /// Thread-local allocation buffer: [top, end) heap offsets.
    /// The thread's TLAB, in its stack region's thread block (null until
    /// the interpreter thread exists).
    pub(crate) tlab: std::sync::atomic::AtomicPtr<crate::memory::ThreadBlock>,
}

// SAFETY: `handles` is only touched by its owner thread or by the collector
// while the owner is stopped (GIL protocol); everything else is synchronized.
unsafe impl Sync for ThreadShared {}

thread_local! {
    /// The ThreadShared of the Java thread running on this OS thread.
    static CURRENT: Cell<*const ThreadShared> = const { Cell::new(std::ptr::null()) };
}

/// Whether this OS thread runs a Java thread (and so holds the GIL when it
/// allocates).
pub(crate) fn on_vm_thread() -> bool {
    CURRENT.with(|c| !c.get().is_null())
}

/// Size of a thread-local allocation buffer.
pub(crate) const TLAB_SIZE: usize = 32 * 1024;

/// Bump-allocates `size` (8-aligned) bytes from the current thread's buffer,
/// refilling it from the heap when exhausted. None if this OS thread is not a
/// Java thread, the object is too large for a buffer, or the heap is full.
#[inline]
pub(crate) fn tlab_alloc(heap: &crate::heap::Heap, size: u32) -> Option<Ref> {
    CURRENT.with(|c| {
        let shared = c.get();
        if shared.is_null() || size as usize > TLAB_SIZE / 4 {
            return None;
        }
        // SAFETY: the current thread's own block (compiled code bumps it too).
        let block = unsafe { (*shared).tlab.load(Ordering::Relaxed).as_ref()? };
        let (mut top, mut end) = (block.tlab_top.load(Ordering::Relaxed), block.tlab_end.load(Ordering::Relaxed));
        if end - top < size {
            (top, end) = heap.alloc_chunk(size as usize, TLAB_SIZE)?;
            block.tlab_end.store(end, Ordering::Relaxed);
        }
        block.tlab_top.store(top + size, Ordering::Relaxed);
        let obj = Ref(top);
        heap.set_start(obj);
        Some(obj)
    })
}

/// Roots a reference created by Rust code until the enclosing native method
/// returns (or the interpreter reaches a safepoint).
pub(crate) fn push_handle(r: Ref) {
    CURRENT.with(|c| {
        let shared = c.get();
        if !shared.is_null() {
            // SAFETY: the current thread's own handle list.
            unsafe { (*(*shared).handles.get()).push(r) }
        }
    });
}

#[derive(Default)]
struct WakeState {
    /// LockSupport permit.
    permit: bool,
    /// Bumped by notify/interrupt so blocked threads notice.
    wakeups: u64,
}

impl ThreadShared {
    pub fn unpark(&self) {
        self.wake.lock().unwrap().permit = true;
        self.cond.notify_all();
    }

    pub fn wake(&self) {
        self.wake.lock().unwrap().wakeups += 1;
        self.cond.notify_all();
    }

    /// Current wakeup count; taken before releasing the GIL so that a wakeup
    /// arriving in between is not lost.
    fn wakeups(&self) -> u64 {
        self.wake.lock().unwrap().wakeups
    }

    /// Blocks until woken (permit, or a wakeup after `since`) or the deadline;
    /// returns whether the permit was consumed.
    fn block(&self, deadline: Option<Instant>, consume_permit: bool, since: u64) -> bool {
        let mut state = self.wake.lock().unwrap();
        let start_wakeups = since;
        loop {
            if consume_permit && state.permit {
                state.permit = false;
                return true;
            }
            if state.wakeups != start_wakeups {
                return false;
            }
            match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return false;
                    }
                    state = self.cond.wait_timeout(state, deadline - now).unwrap().0;
                }
                None => state = self.cond.wait(state).unwrap(),
            }
        }
    }
}

#[derive(Default)]
struct MonitorState {
    owner: u64,
    count: u32,
    /// Threads in wait(), in arrival order.
    waiters: Vec<u64>,
}

/// VM-wide thread registry and object monitors.
pub struct Threads {
    pub safepoints: Safepoints,
    registry: Mutex<HashMap<u64, Arc<ThreadShared>>>,
    next_id: AtomicU64,
    monitors: Mutex<crate::FastMap<Ref, MonitorState>>,
    /// Number of live non-daemon threads (DestroyJavaVM waits for zero).
    non_daemon: Mutex<usize>,
    non_daemon_cond: Condvar,
}

impl Threads {
    /// `stop` is the stop-the-world flag (see `Safepoints`).
    pub fn new(stop: &'static AtomicU32) -> Threads {
        Threads {
            safepoints: Safepoints::new(stop),
            registry: Default::default(),
            next_id: Default::default(),
            monitors: Default::default(),
            non_daemon: Default::default(),
            non_daemon_cond: Default::default(),
        }
    }

    pub fn register(&self) -> Arc<ThreadShared> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let shared = Arc::new(ThreadShared {
            id,
            java_thread: RefCell32::default(),
            wake: Mutex::new(WakeState::default()),
            cond: Condvar::new(),
            handles: UnsafeCell::new(Vec::with_capacity(256)),
            interpreter: AtomicPtr::new(std::ptr::null_mut()),
            tlab: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
        });
        self.registry.lock().unwrap().insert(id, shared.clone());
        shared
    }

    pub fn unregister(&self, id: u64) {
        self.registry.lock().unwrap().remove(&id);
    }

    pub fn get(&self, id: u64) -> Option<Arc<ThreadShared>> {
        self.registry.lock().unwrap().get(&id).cloned()
    }

    pub fn java_threads(&self) -> Vec<Ref> {
        let mut threads: Vec<(u64, Ref)> = self
            .registry
            .lock()
            .unwrap()
            .values()
            .map(|t| (t.id, t.java_thread.get()))
            .filter(|(_, r)| !r.is_null())
            .collect();
        threads.sort();
        threads.into_iter().map(|(_, r)| r).collect()
    }

    /// Calls `f(slot, precise)` for every root held by threads: Thread objects
    /// and handles (precise), interpreter stacks (conservative).
    pub(crate) fn visit_roots(&self, f: &mut dyn FnMut(u64, bool)) {
        for thread in self.registry.lock().unwrap().values() {
            f(thread.java_thread.get().slot(), true);
            // SAFETY: the world is stopped; the owner is not running.
            for handle in unsafe { &*thread.handles.get() } {
                f(handle.slot(), true);
            }
            let interpreter = thread.interpreter.load(Ordering::Acquire) as *const Thread<'static>;
            if !interpreter.is_null() {
                // SAFETY: attached interpreters stay in place until detached.
                unsafe { (*interpreter).visit_stack_roots(f) };
            }
        }
    }

    /// Debugging aid (like jstack): prints every attached thread's Java stack.
    /// Reads other threads' frames without synchronisation, so the output is
    /// a best-effort snapshot.
    /// RJAVA_JSAMPLE=<thread name>[,<interval ms>]: samples the Java stacks of
    /// the threads with that name and prints, every 10 seconds, the methods
    /// with the most samples on the stack (inclusive) and on top (self).
    pub fn start_sampler(vm: &'static crate::Vm) {
        let Ok(spec) = std::env::var("RJAVA_JSAMPLE") else { return };
        let (name, interval) = match spec.split_once(',') {
            Some((name, ms)) => (name.to_owned(), ms.parse().unwrap_or(1)),
            None => (spec, 1),
        };
        crate::memory::SAMPLING.store(true, Ordering::Relaxed);
        // RJAVA_JSAMPLE_GROUPS=<prefix>,...: also the share of samples with
        // a method of a class with that name prefix on the stack.
        let groups: Vec<String> =
            std::env::var("RJAVA_JSAMPLE_GROUPS").map(|g| g.split(',').map(str::to_owned).collect()).unwrap_or_default();
        std::thread::spawn(move || {
            let mut targets: Vec<Arc<ThreadShared>> = Vec::new();
            let mut inclusive: HashMap<usize, u64> = HashMap::new();
            let mut leaf: HashMap<usize, u64> = HashMap::new();
            // (caller, top) pairs.
            let mut pairs: HashMap<(usize, usize), u64> = HashMap::new();
            let mut samples = 0u64;
            let mut group_counts = vec![0u64; groups.len()];
            // For the first group: (caller, first method in the group) where
            // the stack enters it.
            let mut entries: HashMap<(usize, usize), u64> = HashMap::new();
            // Samples with the world stopped (a collection), and with the
            // thread blocked (parked, sleeping or waiting).
            let (mut stopped, mut blocked) = (0u64, 0u64);
            let mut stack = Vec::new();
            let mut last_report = Instant::now();
            let mut last_lookup: Option<Instant> = None;
            loop {
                std::thread::sleep(Duration::from_millis(interval));
                if last_lookup.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) && !vm.threads.safepoints.stopping() {
                    last_lookup = Some(Instant::now());
                    let registry: Vec<Arc<ThreadShared>> = vm.threads.registry.lock().unwrap().values().cloned().collect();
                    targets = registry
                        .into_iter()
                        .filter(|t| {
                            let interpreter = t.interpreter.load(Ordering::Acquire) as *const Thread<'static>;
                            // SAFETY: racy read of a live interpreter (sampling only).
                            !interpreter.is_null() && unsafe { &*interpreter }.thread_name() == name
                        })
                        .collect();
                }
                if vm.threads.safepoints.stopping() {
                    stopped += targets.len() as u64;
                    continue;
                }
                for target in &targets {
                    let interpreter = target.interpreter.load(Ordering::Acquire) as *const Thread<'static>;
                    if interpreter.is_null() {
                        continue;
                    }
                    stack.clear();
                    // SAFETY: as above.
                    unsafe { &*interpreter }.sample_stack(&mut stack);
                    // Only methods the VM knows (a racy read can see garbage).
                    stack.retain(|m| vm.method_if_any(m.global_index).is_some_and(|k| std::ptr::eq(k, *m)));
                    let Some(&top) = stack.last() else { continue };
                    let blocking = match top.class().name.as_str() {
                        "jdk/internal/misc/Unsafe" => top.name == "park",
                        "java/util/concurrent/locks/LockSupport" => top.name.starts_with("park"),
                        "java/lang/Thread" => matches!(top.name.as_str(), "sleep" | "sleepNanos" | "sleepNanos0" | "sleep0" | "yield" | "yield0"),
                        "java/lang/Object" => top.name.starts_with("wait"),
                        _ => false,
                    };
                    if blocking {
                        blocked += 1;
                        continue;
                    }
                    samples += 1;
                    for (count, prefix) in group_counts.iter_mut().zip(&groups) {
                        if stack.iter().any(|m| m.class().name.starts_with(prefix.as_str())) {
                            *count += 1;
                        }
                    }
                    if let Some(first) = groups.first() {
                        if let Some(at) = stack.iter().position(|m| m.class().name.starts_with(first.as_str())).filter(|&i| i > 0) {
                            *entries.entry((stack[at - 1] as *const _ as usize, stack[at] as *const _ as usize)).or_default() += 1;
                        }
                    }
                    *leaf.entry(top as *const _ as usize).or_default() += 1;
                    if stack.len() >= 2 {
                        let caller = stack[stack.len() - 2];
                        *pairs.entry((caller as *const _ as usize, top as *const _ as usize)).or_default() += 1;
                    }
                    stack.sort_by_key(|m| *m as *const _ as usize);
                    stack.dedup_by_key(|m| *m as *const _ as usize);
                    for &m in &stack {
                        *inclusive.entry(m as *const _ as usize).or_default() += 1;
                    }
                }
                if last_report.elapsed() >= Duration::from_secs(10) && samples > 0 {
                    last_report = Instant::now();
                    let all = (samples + stopped + blocked) as f64;
                    eprintln!(
                        "[jsample] \"{name}\": running {:.1}%, blocked {:.1}%, world stopped {:.1}%",
                        samples as f64 * 100.0 / all,
                        blocked as f64 * 100.0 / all,
                        stopped as f64 * 100.0 / all
                    );
                    (stopped, blocked) = (0, 0);
                    let mut rows: Vec<((usize, usize), u64)> = entries.drain().collect();
                    rows.sort_by(|a, b| b.1.cmp(&a.1));
                    for ((caller, first), n) in rows.into_iter().take(20) {
                        // SAFETY: known methods (checked when sampled).
                        let (caller, first) = unsafe { (&*(caller as *const crate::class::Method), &*(first as *const crate::class::Method)) };
                        eprintln!("[jsample] entry {:5.1}%  {caller:?} → {first:?}", n as f64 * 100.0 / samples as f64);
                    }
                    for (count, prefix) in group_counts.iter_mut().zip(&groups) {
                        eprintln!("[jsample] \"{name}\": {:.1}% with {prefix}*", *count as f64 * 100.0 / samples as f64);
                        *count = 0;
                    }
                    let show = |title: &str, table: &mut HashMap<usize, u64>, top: usize| {
                        let mut rows: Vec<(usize, u64)> = table.drain().collect();
                        rows.sort_by(|a, b| b.1.cmp(&a.1));
                        let mut out = format!("[jsample] \"{name}\" {title} ({samples} samples)\n");
                        for (method, n) in rows.into_iter().take(top) {
                            // SAFETY: a known method (checked above).
                            let method = unsafe { &*(method as *const crate::class::Method) };
                            out += &format!("  {:5.1}%  {method:?}\n", n as f64 * 100.0 / samples as f64);
                        }
                        eprint!("{out}");
                    };
                    show("inclusive", &mut inclusive, 45);
                    show("self", &mut leaf, 25);
                    let mut rows: Vec<((usize, usize), u64)> = pairs.drain().collect();
                    rows.sort_by(|a, b| b.1.cmp(&a.1));
                    let mut out = format!("[jsample] \"{name}\" caller → top\n");
                    for ((caller, top), n) in rows.into_iter().take(15) {
                        // SAFETY: known methods (checked above).
                        let (caller, top) = unsafe { (&*(caller as *const crate::class::Method), &*(top as *const crate::class::Method)) };
                        out += &format!("  {:5.1}%  {caller:?} → {top:?}\n", n as f64 * 100.0 / samples as f64);
                    }
                    eprint!("{out}");
                    samples = 0;
                }
            }
        });
    }

    pub fn dump_stacks(&self) {
        let threads: Vec<Arc<ThreadShared>> = self.registry.lock().unwrap().values().cloned().collect();
        let mut out = String::from("=== thread dump ===\n");
        for thread in threads {
            let interpreter = thread.interpreter.load(Ordering::Acquire) as *const Thread<'static>;
            if interpreter.is_null() {
                continue;
            }
            // SAFETY: racy read of a live, attached interpreter (debugging only).
            let interpreter = unsafe { &*interpreter };
            let frames = interpreter.stack_trace();
            out.push_str(&format!("\"{}\" #{} ({} frames)\n", interpreter.thread_name(), thread.id, frames.len()));
            for (method, pc) in frames.iter().take(30) {
                out.push_str(&format!("    {method:?} @{pc} line {:?}\n", method.line_number(*pc)));
            }
        }
        for (obj, monitor) in self.monitors.lock().unwrap().iter() {
            if monitor.owner != 0 || !monitor.waiters.is_empty() {
                out.push_str(&format!("monitor {obj:?}: owner #{} count {} waiters {:?}\n", monitor.owner, monitor.count, monitor.waiters));
            }
        }
        eprint!("{out}");
    }

    /// Abandons every thread's allocation buffer (before the sweep rebuilds the
    /// free lists, which may include their unused space).
    pub(crate) fn reset_tlabs(&self) {
        for thread in self.registry.lock().unwrap().values() {
            // SAFETY: the world is stopped.
            if let Some(block) = unsafe { thread.tlab.load(Ordering::Relaxed).as_ref() } {
                block.tlab_top.store(0, Ordering::Relaxed);
                block.tlab_end.store(0, Ordering::Relaxed);
            }
        }
    }

    /// Objects with monitor records (pinned by the collector: they are keys).
    pub(crate) fn visit_monitor_keys(&self, f: &mut dyn FnMut(Ref)) {
        for &obj in self.monitors.lock().unwrap().keys() {
            f(obj);
        }
    }

    /// Drops monitor records of objects that died (and are unowned).
    pub(crate) fn prune_monitors(&self, alive: impl Fn(Ref) -> bool) {
        self.monitors.lock().unwrap().retain(|&obj, m| alive(obj) || m.owner != 0 || !m.waiters.is_empty());
    }

    pub fn non_daemon_started(&self) {
        *self.non_daemon.lock().unwrap() += 1;
    }

    pub fn non_daemon_finished(&self) {
        *self.non_daemon.lock().unwrap() -= 1;
        self.non_daemon_cond.notify_all();
    }

    fn wait_for_non_daemon(&self) {
        let mut count = self.non_daemon.lock().unwrap();
        while *count > 0 {
            count = self.non_daemon_cond.wait(count).unwrap();
        }
    }
}

/// JVMTI thread states, as stored in Thread.FieldHolder.threadStatus.
pub mod status {
    pub const RUNNABLE: i32 = 0x0005;
    pub const TERMINATED: i32 = 0x0002;
    pub const BLOCKED_ON_MONITOR_ENTER: i32 = 0x0401;
    pub const WAITING: i32 = 0x0191;
    pub const TIMED_WAITING: i32 = 0x01A1;
    pub const PARKED: i32 = 0x0291;
    pub const PARKED_TIMED: i32 = 0x02A1;
    pub const SLEEPING: i32 = 0x00E1;
}

impl ThreadShared {
    pub(crate) fn handles_len(&self) -> usize {
        // SAFETY: called by the owning thread.
        unsafe { (*self.handles.get()).len() }
    }

    pub(crate) fn truncate_handles(&self, len: usize) {
        // SAFETY: called by the owning thread.
        unsafe { (*self.handles.get()).truncate(len) }
    }

    pub(crate) fn make_current(&self) {
        CURRENT.with(|c| c.set(self as *const ThreadShared));
    }
}

impl Thread<'_> {
    /// Makes this interpreter's stack visible to the collector. Call once the
    /// Thread is at its final location; `detach` before it is dropped.
    pub fn attach(&mut self) {
        if let Some(bytes) = self.vm.jit.as_ref().and_then(|jit| jit.native_stack()) {
            let budget = bytes.saturating_sub(crate::jit::NATIVE_RESERVE).max(64 << 10);
            self.thread_block().native_budget.store(budget as i32, Ordering::Relaxed);
        }
        self.shared.make_current();
        let this = self as *mut Thread<'_> as *mut ();
        self.shared.interpreter.store(this, Ordering::Release);
    }

    pub fn detach(&mut self) {
        self.shared.interpreter.store(std::ptr::null_mut(), Ordering::Release);
        self.vm.threads.unregister(self.shared.id);
        CURRENT.with(|c| c.set(std::ptr::null()));
    }

    /// Runs `f` without the GIL (blocking operations).
    pub fn blocking<R>(&mut self, f: impl FnOnce() -> R) -> R {
        self.vm.threads.safepoints.leave();
        let result = f();
        self.vm.threads.safepoints.enter();
        result
    }

    /// Lets waiting threads run (a safepoint).
    pub fn yield_now(&mut self) {
        self.vm.threads.safepoints.poll();
    }

    pub(crate) fn set_thread_status(&self, status: i32) {
        let thread = self.java_thread;
        if thread.is_null() {
            return;
        }
        let holder = self.field_ref(thread, "java/lang/Thread", "holder");
        if !holder.is_null() {
            self.set_field(holder, "java/lang/Thread$FieldHolder", "threadStatus", status as u32 as u64);
        }
    }

    /// The java.lang.Thread name (for diagnostics).
    pub fn thread_name(&self) -> String {
        if self.java_thread.is_null() {
            return "<unnamed>".into();
        }
        let name = self.field_ref(self.java_thread, "java/lang/Thread", "name");
        if name.is_null() { "<unnamed>".into() } else { self.vm.string_value(name) }
    }

    pub fn is_interrupted(&self) -> bool {
        !self.java_thread.is_null() && self.field(self.java_thread, "java/lang/Thread", "interrupted") != 0
    }

    fn take_interrupt(&self) -> bool {
        let interrupted = self.is_interrupted();
        if interrupted {
            self.set_field(self.java_thread, "java/lang/Thread", "interrupted", 0);
        }
        interrupted
    }

    // --- monitors -------------------------------------------------------------------

    pub fn monitor_enter(&mut self, obj: Ref) -> JResult<()> {
        let me = self.shared.id;
        let mut spins = 0u32;
        loop {
            {
                let mut monitors = self.vm.threads.monitors.lock().unwrap();
                let monitor = monitors.entry(obj).or_default();
                if monitor.owner == 0 || monitor.owner == me {
                    monitor.owner = me;
                    monitor.count += 1;
                    if spins > 0 {
                        self.set_thread_status(status::RUNNABLE);
                    }
                    return Ok(());
                }
            }
            if spins == 0 {
                self.set_thread_status(status::BLOCKED_ON_MONITOR_ENTER);
            }
            spins += 1;
            // Spin briefly, then back off.
            let backoff = Duration::from_micros(if spins < 16 { 0 } else { 200 });
            self.blocking(|| {
                if !backoff.is_zero() {
                    std::thread::sleep(backoff);
                }
            });
        }
    }

    pub fn monitor_exit(&mut self, obj: Ref) -> JResult<()> {
        let me = self.shared.id;
        let mut monitors = self.vm.threads.monitors.lock().unwrap();
        match monitors.get_mut(&obj) {
            Some(monitor) if monitor.owner == me => {
                monitor.count -= 1;
                if monitor.count == 0 {
                    monitor.owner = 0;
                    if monitor.waiters.is_empty() {
                        monitors.remove(&obj);
                    }
                }
                Ok(())
            }
            _ => {
                drop(monitors);
                Err(self.throw_new("java/lang/IllegalMonitorStateException", Some("current thread is not owner")))
            }
        }
    }

    pub fn holds_lock(&self, obj: Ref) -> bool {
        self.vm.threads.monitors.lock().unwrap().get(&obj).is_some_and(|m| m.owner == self.shared.id)
    }

    /// Object.wait: releases the monitor, waits for notify/timeout/interrupt,
    /// then reacquires it.
    pub fn monitor_wait(&mut self, obj: Ref, millis: i64) -> JResult<()> {
        if millis < 0 {
            return Err(self.throw_new("java/lang/IllegalArgumentException", Some("timeout value is negative")));
        }
        let me = self.shared.id;
        let since = self.shared.wakeups();
        let count = {
            let mut monitors = self.vm.threads.monitors.lock().unwrap();
            match monitors.get_mut(&obj) {
                Some(monitor) if monitor.owner == me => {
                    let count = monitor.count;
                    monitor.owner = 0;
                    monitor.count = 0;
                    monitor.waiters.push(me);
                    count
                }
                _ => {
                    drop(monitors);
                    return Err(self.throw_new("java/lang/IllegalMonitorStateException", Some("current thread is not owner")));
                }
            }
        };
        if self.take_interrupt() {
            self.remove_waiter(obj);
            self.reenter(obj, count)?;
            return Err(self.throw_new("java/lang/InterruptedException", None));
        }
        let deadline = (millis > 0).then(|| Instant::now() + Duration::from_millis(millis as u64));
        self.set_thread_status(if deadline.is_some() { status::TIMED_WAITING } else { status::WAITING });
        let shared = self.shared.clone();
        self.blocking(|| shared.block(deadline, false, since));
        self.remove_waiter(obj);
        self.reenter(obj, count)?;
        self.set_thread_status(status::RUNNABLE);
        if self.take_interrupt() {
            return Err(self.throw_new("java/lang/InterruptedException", None));
        }
        Ok(())
    }

    fn remove_waiter(&self, obj: Ref) {
        let me = self.shared.id;
        if let Some(monitor) = self.vm.threads.monitors.lock().unwrap().get_mut(&obj) {
            monitor.waiters.retain(|&w| w != me);
        }
    }

    fn reenter(&mut self, obj: Ref, count: u32) -> JResult<()> {
        self.monitor_enter(obj)?;
        if let Some(monitor) = self.vm.threads.monitors.lock().unwrap().get_mut(&obj) {
            monitor.count = count;
        }
        Ok(())
    }

    pub fn monitor_notify(&mut self, obj: Ref, all: bool) -> JResult<()> {
        let me = self.shared.id;
        let woken: Vec<u64> = {
            let mut monitors = self.vm.threads.monitors.lock().unwrap();
            match monitors.get_mut(&obj) {
                Some(monitor) if monitor.owner == me => {
                    if all {
                        std::mem::take(&mut monitor.waiters)
                    } else if monitor.waiters.is_empty() {
                        Vec::new()
                    } else {
                        vec![monitor.waiters.remove(0)]
                    }
                }
                _ => {
                    drop(monitors);
                    return Err(self.throw_new("java/lang/IllegalMonitorStateException", Some("current thread is not owner")));
                }
            }
        };
        for id in woken {
            if let Some(thread) = self.vm.threads.get(id) {
                thread.wake();
            }
        }
        Ok(())
    }

    // --- parking and sleeping -------------------------------------------------------

    /// Unsafe.park: `time` is nanoseconds (relative) or epoch milliseconds (absolute).
    pub fn park(&mut self, absolute: bool, time: i64) {
        if self.is_interrupted() {
            return;
        }
        let deadline = if absolute {
            let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
            if time <= now_ms {
                return;
            }
            Some(Instant::now() + Duration::from_millis((time - now_ms) as u64))
        } else if time > 0 {
            Some(Instant::now() + Duration::from_nanos(time as u64))
        } else if time == 0 {
            None
        } else {
            return;
        };
        self.set_thread_status(if deadline.is_some() { status::PARKED_TIMED } else { status::PARKED });
        let shared = self.shared.clone();
        let since = shared.wakeups();
        self.blocking(|| shared.block(deadline, true, since));
        self.set_thread_status(status::RUNNABLE);
    }

    /// Blocks until another thread wakes this one (interrupt).
    pub fn wait_indefinitely(&mut self) {
        let shared = self.shared.clone();
        let since = shared.wakeups();
        self.set_thread_status(status::WAITING);
        self.blocking(|| shared.block(None, false, since));
        self.set_thread_status(status::RUNNABLE);
    }

    pub fn sleep_nanos(&mut self, nanos: i64) -> JResult<()> {
        if self.take_interrupt() {
            return Err(self.throw_new("java/lang/InterruptedException", Some("sleep interrupted")));
        }
        if nanos > 0 {
            let deadline = Instant::now() + Duration::from_nanos(nanos as u64);
            self.set_thread_status(status::SLEEPING);
            let shared = self.shared.clone();
            let since = shared.wakeups();
            self.blocking(|| shared.block(Some(deadline), false, since));
            self.set_thread_status(status::RUNNABLE);
        } else {
            self.yield_now();
        }
        if self.take_interrupt() {
            return Err(self.throw_new("java/lang/InterruptedException", Some("sleep interrupted")));
        }
        Ok(())
    }

    // --- lifecycle --------------------------------------------------------------------

    /// Thread.start0: runs `thread.run()` on a new OS thread.
    pub fn start_thread(&mut self, thread: Ref) -> JResult<()> {
        let vm = self.vm.as_static();
        let holder = self.field_ref(thread, "java/lang/Thread", "holder");
        let daemon = self.field(holder, "java/lang/Thread$FieldHolder", "daemon") != 0;
        let name = self.field_ref(thread, "java/lang/Thread", "name");
        let name = if name.is_null() { "Thread".to_owned() } else { vm.string_value(name) };
        let Some(stack) = vm.memory.take_stack() else {
            return Err(self.throw_new("java/lang/OutOfMemoryError", Some("unable to create native thread: no free stack")));
        };
        let shared = vm.threads.register();
        shared.java_thread.set(thread);
        self.set_field(thread, "java/lang/Thread", "eetop", shared.id);
        self.set_field(holder, "java/lang/Thread$FieldHolder", "threadStatus", status::RUNNABLE as u64);
        if !daemon {
            vm.threads.non_daemon_started();
        }
        let spawned = std::thread::Builder::new().name(name).stack_size(if cfg!(target_family = "wasm") { 8 << 20 } else { 64 << 20 }).spawn(move || {
            vm.threads.safepoints.enter();
            let mut t = Thread::with_shared(vm, shared, stack);
            t.attach();
            t.java_thread = thread;
            if let Err(crate::interp::Throw(exception)) = t.invoke_virtual(thread, "run", "()V", &[]) {
                let _ = t.invoke_virtual(thread, "dispatchUncaughtException", "(Ljava/lang/Throwable;)V", &[exception.slot()]);
            }
            t.terminate(daemon);
            vm.threads.safepoints.leave();
        });
        if let Err(e) = spawned {
            return Err(self.throw_new("java/lang/OutOfMemoryError", Some(&format!("unable to create native thread: {e}"))));
        }
        Ok(())
    }

    /// Thread exit (HotSpot's JavaThread::exit + ensure_join).
    fn terminate(&mut self, daemon: bool) {
        let thread = self.java_thread;
        let _ = self.invoke_virtual(thread, "exit", "()V", &[]);
        if self.monitor_enter(thread).is_ok() {
            self.set_thread_status(status::TERMINATED);
            self.set_field(thread, "java/lang/Thread", "eetop", 0);
            let _ = self.monitor_notify(thread, true);
            let _ = self.monitor_exit(thread);
        }
        self.detach();
        if !daemon {
            self.vm.threads.non_daemon_finished();
        }
    }

    /// DestroyJavaVM: waits for all non-daemon threads, then runs shutdown hooks.
    pub fn destroy_vm(&mut self) -> JResult<()> {
        let vm = self.vm;
        self.blocking(|| vm.threads.wait_for_non_daemon());
        self.invoke_static("java/lang/Shutdown", "shutdown", "()V", &[])?;
        Ok(())
    }
}
