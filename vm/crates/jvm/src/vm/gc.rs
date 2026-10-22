//! A stop-the-world mark-sweep collector that compacts when free space gets
//! fragmented (sliding mark-compact with pinning).
//!
//! Roots: everything the VM itself holds (class mirrors, statics, loaders,
//! interned strings, linked constants and call sites, modules), each thread's
//! Thread object and handles (references created by Rust code that has not
//! returned yet), and each thread's interpreter stack. Interpreter slots are
//! untyped, so they are scanned conservatively: a slot that holds the address
//! of an allocated object keeps it alive and pins it. Pinned objects (those
//! reachable from roots that cannot be rewritten) stay in place during a
//! compaction; everything else slides down and references to it are updated.
//!
//! The collector stops the world (see `threads::Safepoints`): every other
//! thread is then parked at a safepoint or inside a blocking operation.
//!
//! Not yet implemented: reference processing (soft/weak/phantom references
//! are treated as strong) and finalization.

use super::{STATICS_MARK, Vm};
use std::sync::Mutex;
use crate::class::{ClassKind, ClassRef, FieldKind};
use crate::heap::{ARRAY_BASE_OFFSET, Compaction, HEADER_SIZE, Heap, Ref};
use std::sync::atomic::Ordering;

struct Marker<'a> {
    heap: &'a Heap,
    work: Vec<Ref>,
}

impl Marker<'_> {
    #[inline]
    fn root(&mut self, r: Ref) {
        if self.heap.is_object(r) && self.heap.mark(r) {
            self.work.push(r);
        }
    }

    /// A root whose holder cannot be updated, so the object must not move.
    #[inline]
    fn pin(&mut self, r: Ref) {
        if self.heap.is_object(r) {
            self.heap.pin(r);
            self.root(r);
        }
    }

    /// A slot that may or may not hold a reference (pins what it points at).
    #[inline]
    fn conservative(&mut self, slot: u64) {
        if slot >> 32 == 0 {
            self.pin(Ref(slot as u32));
        }
    }
}

/// Threads marking in parallel (each OS thread scans objects; mark bits are
/// set atomically).
fn gc_threads() -> usize {
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *THREADS.get_or_init(|| {
        std::env::var("RJAVA_GC_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| crate::processors().min(8))
            .max(1)
    })
}

/// Work shared between marking threads: chunks of grey objects.
struct MarkPool {
    chunks: Mutex<Vec<Vec<Ref>>>,
    /// Threads with no work; all of them idle with no chunks left: done.
    idle: std::sync::atomic::AtomicUsize,
}

impl Vm {
    /// Calls `f` for every reference field of `obj`.
    #[inline]
    fn for_each_reference(&self, classes: &[ClassRef], obj: Ref, mut f: impl FnMut(Ref)) {
        let class = classes[self.heap.class_id(obj) as usize];
        if self.heap.read::<u32>(obj, 4) == STATICS_MARK {
            for &offset in &class.static_ref_offsets {
                f(Ref(self.heap.read(obj, offset)));
            }
            return;
        }
        match class.kind {
            ClassKind::Array { .. } => {
                if class.element_kind() == Some(FieldKind::Ref) {
                    let len = self.heap.array_length(obj) as u32;
                    for i in 0..len {
                        f(Ref(self.heap.read(obj, ARRAY_BASE_OFFSET + i * 4)));
                    }
                }
            }
            ClassKind::Instance => {
                for &offset in &class.ref_offsets {
                    f(Ref(self.heap.read(obj, offset)));
                }
            }
            ClassKind::Primitive(_) => {}
        }
    }

    /// Marks everything reachable from the (marked) grey objects `grey`, on
    /// several threads that share work in chunks.
    fn mark_from(&self, classes: &[ClassRef], grey: Vec<Ref>) {
        const CHUNK: usize = 256;
        let threads = gc_threads();
        let heap = &self.heap;
        if threads == 1 {
            let mut work = grey;
            while let Some(obj) = work.pop() {
                self.for_each_reference(classes, obj, |r| {
                    if heap.is_object(r) && heap.mark(r) {
                        work.push(r);
                    }
                });
            }
            return;
        }
        let pool = MarkPool { chunks: Mutex::new(grey.chunks(CHUNK).map(<[Ref]>::to_vec).collect()), idle: Default::default() };
        let relaxed = std::sync::atomic::Ordering::SeqCst;
        crate::pool::run(threads, |_| {
            let mut work: Vec<Ref> = Vec::with_capacity(4 * CHUNK);
            loop {
                if work.is_empty() {
                    // (Not in the match scrutinee: that would hold the lock.)
                    let chunk = pool.chunks.lock().unwrap().pop();
                    match chunk {
                        Some(chunk) => work = chunk,
                        None => {
                            // Idle until someone shares work or all are idle.
                            pool.idle.fetch_add(1, relaxed);
                            loop {
                                if pool.idle.load(relaxed) == threads && pool.chunks.lock().unwrap().is_empty() {
                                    return;
                                }
                                let chunk = pool.chunks.lock().unwrap().pop();
                                if let Some(chunk) = chunk {
                                    pool.idle.fetch_sub(1, relaxed);
                                    work = chunk;
                                    break;
                                }
                                std::thread::yield_now();
                            }
                        }
                    }
                }
                while let Some(obj) = work.pop() {
                    self.for_each_reference(classes, obj, |r| {
                        if heap.is_object(r) && heap.mark(r) {
                            work.push(r);
                        }
                    });
                    // Share when others may be starving.
                    if work.len() >= 2 * CHUNK && pool.idle.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                        let shared = work.split_off(work.len() - CHUNK);
                        pool.chunks.lock().unwrap().push(shared);
                    }
                }
            }
        });
    }

    /// Collects garbage, stopping every other Java thread first (unless
    /// another thread is already collecting: then this one just waits).
    pub fn collect(&self) {
        let started = std::time::Instant::now();
        if !self.threads.safepoints.stop_the_world() {
            return;
        }
        if std::env::var_os("RJAVA_GC_LOG").is_some() {
            eprintln!("[gc] world stopped in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        }
        let verify = std::env::var_os("RJAVA_GC_VERIFY").is_some();
        if verify {
            self.verify_heap("before collection");
        }
        self.mark_and_sweep();
        if verify {
            self.verify_heap("after collection");
        }
        self.threads.safepoints.resume();
    }

    /// Checks every object: a valid class, a sane array length, and reference
    /// fields that are null or point at allocated objects. Panics on the
    /// first violation (RJAVA_GC_VERIFY).
    pub fn verify_heap(&self, when: &str) {
        let classes = self.classes.all();
        let heap = &self.heap;
        let check = |holder: Ref, what: &str, value: Ref| {
            if !value.is_null() && !heap.is_object(value) {
                let class = classes[heap.class_id(holder) as usize];
                panic!("heap verification failed {when}: {class:?} object {holder:?} {what} = {value:?} is not an object");
            }
        };
        heap.for_each_object(|obj| {
            let id = heap.class_id(obj) as usize;
            if id == 0 || id >= classes.len() {
                panic!("heap verification failed {when}: object {obj:?} has invalid class id {id:#x}");
            }
            let class = classes[id];
            if heap.read::<u32>(obj, 4) == STATICS_MARK {
                for &offset in &class.static_ref_offsets {
                    check(obj, "static field", Ref(heap.read(obj, offset)));
                }
                return object_size(heap, class, obj);
            }
            match class.kind {
                ClassKind::Array { .. } => {
                    let len = heap.array_length(obj);
                    if len < 0 {
                        panic!("heap verification failed {when}: {class:?} {obj:?} has length {len}");
                    }
                    if class.element_kind() == Some(FieldKind::Ref) {
                        for i in 0..len as u32 {
                            check(obj, "element", Ref(heap.read(obj, ARRAY_BASE_OFFSET + i * 4)));
                        }
                    }
                }
                ClassKind::Instance => {
                    for &offset in &class.ref_offsets {
                        check(obj, "field", Ref(heap.read(obj, offset)));
                    }
                }
                ClassKind::Primitive(_) => panic!("heap verification failed {when}: instance of primitive class at {obj:?}"),
            }
            object_size(heap, class, obj)
        });
    }

    fn mark_and_sweep(&self) {
        let started = std::time::Instant::now();
        let before = self.heap.used();
        let classes = self.classes.all();
        let mut marker = Marker { heap: &self.heap, work: Vec::with_capacity(1 << 14) };

        // Roots the collector can update when objects move.
        for class in classes.iter().skip(1) {
            marker.root(class.mirror.get());
            marker.root(class.statics.get());
        }
        for &constant in self.constants.lock().unwrap().values() {
            marker.root(constant);
        }
        for site in self.indy_sites.lock().unwrap().values().chain(self.handle_sites.lock().unwrap().values()) {
            if let Some(appendix) = site.appendix {
                marker.root(appendix.get());
            }
        }
        for &module in self.boot_modules.lock().unwrap().values() {
            marker.root(module);
        }
        for &module in self.package_modules.lock().unwrap().values() {
            marker.root(module);
        }
        marker.root(self.boot_unnamed_module.get());
        // Roots held where they cannot be updated: pinned.
        for class in classes.iter().skip(1) {
            marker.pin(class.loader);
        }
        for &string in self.strings.lock().unwrap().values() {
            marker.pin(string); // also cached in constant pool caches
        }
        for &(loader, _) in self.package_modules.lock().unwrap().keys() {
            marker.pin(loader);
        }
        for &(loader, _) in self.loader_classes.lock().unwrap().keys() {
            marker.pin(loader);
        }
        self.threads.visit_monitor_keys(&mut |obj| marker.pin(obj));
        #[cfg(unix)]
        self.jni.visit_globals(&mut |obj| marker.pin(obj));
        #[cfg(target_family = "wasm")]
        crate::web::visit_globals(&mut |obj| marker.pin(obj));
        // Thread roots: Rust locals may hold copies of handles and Thread
        // fields, and stack slots are conservative, so all of them are pinned.
        self.threads.visit_roots(&mut |slot, precise| {
            if precise { marker.pin(Ref::from_slot(slot)) } else { marker.conservative(slot) }
        });

        let roots_done = started.elapsed();
        self.mark_from(&classes, std::mem::take(&mut marker.work));

        let mark_done = started.elapsed();
        self.threads.reset_tlabs();
        let size_of = |obj: Ref| object_size(&self.heap, classes[self.heap.class_id(obj) as usize], obj);
        let mut moved = 0;
        let mut phases = String::new();
        let live = if self.heap.should_compact() || std::env::var_os("RJAVA_GC_COMPACT").is_some() {
            let plan = self.heap.plan_compaction(size_of);
            let planned = started.elapsed();
            moved = plan.moved();
            self.update_references(&classes, &plan);
            let updated = started.elapsed();
            let live = self.heap.compact(&plan);
            phases = format!(
                " [plan {:.1}, update {:.1}, move {:.1}]",
                (planned - mark_done).as_secs_f64() * 1000.0,
                (updated - planned).as_secs_f64() * 1000.0,
                (started.elapsed() - updated).as_secs_f64() * 1000.0
            );
            live
        } else {
            self.heap.sweep(size_of)
        };
        drop(classes);
        self.threads.prune_monitors(|r| self.heap.is_object(r));
        self.gc_count.fetch_add(1, Ordering::Relaxed);
        if std::env::var_os("RJAVA_GC_LOG").is_some() {
            let (chunks, largest, bump) = self.heap.free_stats();
            eprintln!(
                "[gc #{}] {} MiB -> {} MiB live, {} MiB free ({chunks} chunks, largest {} KiB, bump {} MiB){}, {:.1} ms (roots {:.1}, mark {:.1}, sweep {:.1}{phases})",
                self.gc_count.load(Ordering::Relaxed),
                before >> 20,
                live >> 20,
                self.heap.free_bytes() >> 20,
                largest >> 10,
                bump >> 20,
                if moved > 0 { format!(", compacted ({moved} objects moved)") } else { String::new() },
                started.elapsed().as_secs_f64() * 1000.0,
                roots_done.as_secs_f64() * 1000.0,
                (mark_done - roots_done).as_secs_f64() * 1000.0,
                (started.elapsed() - mark_done).as_secs_f64() * 1000.0,
            );
        }
    }
}

impl Vm {
    /// Rewrites every reference to a moving object: in live objects (still at
    /// their old addresses) and in the VM's updatable roots.
    fn update_references(&self, classes: &[ClassRef], plan: &Compaction) {
        let heap = &self.heap;
        let update = |obj: Ref, offset: u32| {
            let old = Ref(heap.read(obj, offset));
            if !old.is_null() {
                heap.write(obj, offset, plan.forward(old).0);
            }
        };
        // In parallel, over chunks of objects (each object's fields are
        // written by one thread).
        const CHUNK: usize = 4096;
        let objects = plan.objects();
        let next = std::sync::atomic::AtomicUsize::new(0);
        crate::pool::run(gc_threads(), |_| loop {
            let start = next.fetch_add(CHUNK, Ordering::Relaxed);
            if start >= objects.len() {
                break;
            }
            for &(addr, _) in &objects[start..(start + CHUNK).min(objects.len())] {
                let obj = Ref(addr);
                let class = classes[heap.class_id(obj) as usize];
                if heap.read::<u32>(obj, 4) == STATICS_MARK {
                    for &offset in &class.static_ref_offsets {
                        update(obj, offset);
                    }
                    continue;
                }
                match class.kind {
                    ClassKind::Array { .. } if class.element_kind() == Some(FieldKind::Ref) => {
                        for i in 0..heap.array_length(obj) as u32 {
                            update(obj, ARRAY_BASE_OFFSET + i * 4);
                        }
                    }
                    ClassKind::Instance => {
                        for &offset in &class.ref_offsets {
                            update(obj, offset);
                        }
                    }
                    _ => {}
                }
            }
        });
        for class in classes.iter().skip(1) {
            class.mirror.set(plan.forward(class.mirror.get()));
            class.statics.set(plan.forward(class.statics.get()));
        }
        for constant in self.constants.lock().unwrap().values_mut() {
            *constant = plan.forward(*constant);
        }
        for site in self.indy_sites.lock().unwrap().values_mut().chain(self.handle_sites.lock().unwrap().values_mut()) {
            if let Some(appendix) = site.appendix {
                appendix.set(plan.forward(appendix.get()));
            }
        }
        for module in self.boot_modules.lock().unwrap().values_mut() {
            *module = plan.forward(*module);
        }
        for module in self.package_modules.lock().unwrap().values_mut() {
            *module = plan.forward(*module);
        }
        self.boot_unnamed_module.set(plan.forward(self.boot_unnamed_module.get()));
    }
}

/// Size of an allocated object or statics block.
pub(crate) fn object_size(heap: &Heap, class: ClassRef, obj: Ref) -> u32 {
    if heap.read::<u32>(obj, 4) == STATICS_MARK {
        return HEADER_SIZE + class.statics_size;
    }
    match class.element_kind() {
        Some(kind) => ARRAY_BASE_OFFSET + kind.size() * heap.array_length(obj) as u32,
        None => class.instance_size,
    }
}
