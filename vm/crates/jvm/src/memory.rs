//! The VM's linear memory: one contiguous region holding the Java heap and
//! the threads' interpreter stacks.
//!
//! Compiled code (WebAssembly) addresses this region directly, so it must be
//! a single block: natively it is a wasmtime shared memory, in the browser a
//! range of the VM module's own memory. Without a JIT the VM allocates it.

use std::alloc::{alloc_zeroed, dealloc};
use std::sync::Mutex;

use crate::class::MethodRef;
use crate::heap::{Ref, RefCell32};

/// Slots (8 bytes each) per thread stack: 2 MiB, shared by the value stack
/// (growing up) and the frame records (growing down from the end).
pub const STACK_SLOTS: usize = 256 * 1024;
const STACK_BYTES: usize = STACK_SLOTS * 8;

/// A block of memory provided by the embedder (e.g. a wasmtime memory).
#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub base: *mut u8,
    pub size: usize,
}

// SAFETY: the region is plain memory shared by all VM threads.
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

/// Classes the VM can define (class ids are below this).
pub const MAX_CLASSES: usize = 1 << 20;
/// Per-class metadata compiled code reads: the statics block reference (u32)
/// and the initialised flag (u8), at `meta + id * CLASS_META_SIZE`.
pub const CLASS_META_SIZE: usize = 8;
/// Virtual dispatch cache shared by compiled code: direct-mapped entries of
/// (receiver class id, resolved method index, target method index or -1).
pub const DISPATCH_ENTRIES: usize = 1 << 16;
pub const DISPATCH_ENTRY_SIZE: usize = 16;
/// VM-wide words compiled code reads (the stop-the-world flag).
const VM_BLOCK_SIZE: usize = 64;
const META_BYTES: usize = MAX_CLASSES * CLASS_META_SIZE + DISPATCH_ENTRIES * DISPATCH_ENTRY_SIZE + VM_BLOCK_SIZE;
pub const STACK_REGION_BYTES: usize = STACK_BYTES;

/// Bytes at the start of each thread stack region for per-thread data that
/// compiled code reads (see `ThreadBlock`); regions are STACK_BYTES-aligned,
/// so `record & !(STACK_BYTES - 1)` finds it from a frame record.
pub const THREAD_BLOCK_SIZE: usize = 64;

/// Per-thread data in linear memory: the thread-local allocation buffer and
/// the native stack budget.
#[repr(C)]
pub struct ThreadBlock {
    /// Next free byte (heap offset) and end of the TLAB.
    pub tlab_top: std::sync::atomic::AtomicU32,
    pub tlab_end: std::sync::atomic::AtomicU32,
    /// Bytes of native (engine) stack compiled code may still use; calls past
    /// it run interpreted (see `jit::compile::call_entry`). Unlimited unless
    /// the backend reports a stack size.
    pub native_budget: std::sync::atomic::AtomicI32,
}

/// Offset of `ThreadBlock::native_budget`.
pub const NATIVE_BUDGET_OFFSET: usize = 8;

/// The region and how it is divided: the Java heap, the class metadata
/// table, the dispatch cache, the heap's object-start bitmap, then (aligned)
/// the thread stacks.
pub struct LinearMemory {
    region: Region,
    heap_size: usize,
    owned: bool,
    free_stacks: Mutex<Vec<usize>>,
}

/// Offsets of the parts after the heap.
struct Layout {
    starts: usize,
    stacks: usize,
}

impl Layout {
    fn of(heap_size: usize) -> Layout {
        let starts = heap_size + META_BYTES;
        let stacks = (starts + crate::heap::Heap::starts_bytes(heap_size)).next_multiple_of(STACK_BYTES);
        Layout { starts, stacks }
    }
}

impl LinearMemory {
    /// Bytes needed for a heap of `heap_size` plus `threads` stacks.
    pub fn required_size(heap_size: usize, threads: usize) -> usize {
        Layout::of(heap_size).stacks + threads * STACK_BYTES
    }

    /// Allocates a zeroed region (no JIT, or tests).
    pub fn allocate(heap_size: usize, threads: usize) -> LinearMemory {
        let size = Self::required_size(heap_size, threads);
        let layout = std::alloc::Layout::from_size_align(size, 4096).expect("memory layout");
        // SAFETY: non-zero layout; zeroed memory is committed lazily by the OS.
        let base = unsafe { alloc_zeroed(layout) };
        assert!(!base.is_null(), "cannot reserve {size} bytes");
        let mut memory = Self::new(Region { base, size }, heap_size);
        memory.owned = true;
        memory
    }

    /// Uses an embedder-provided, zeroed region.
    pub fn new(region: Region, heap_size: usize) -> LinearMemory {
        let stacks_start = Layout::of(heap_size).stacks;
        assert!(stacks_start <= region.size, "heap does not fit the memory region");
        let stacks = (region.size - stacks_start) / STACK_BYTES;
        let free_stacks = (0..stacks).rev().map(|i| stacks_start + i * STACK_BYTES).collect();
        LinearMemory { region, heap_size, owned: false, free_stacks: Mutex::new(free_stacks) }
    }

    pub fn region(&self) -> Region {
        self.region
    }

    pub fn heap_base(&self) -> *mut u8 {
        self.region.base
    }

    pub fn heap_size(&self) -> usize {
        self.heap_size
    }

    /// The heap's object-start bitmap (see `Heap::in_region`).
    pub fn starts_bitmap(&self) -> *mut u8 {
        // SAFETY: inside the region.
        unsafe { self.region.base.add(self.starts_bitmap_offset() as usize) }
    }

    pub fn starts_bitmap_offset(&self) -> u32 {
        Layout::of(self.heap_size).starts as u32
    }

    /// Linear-memory offset of the stop-the-world flag (a u32).
    pub fn stop_flag_offset(&self) -> u32 {
        (self.heap_size + MAX_CLASSES * CLASS_META_SIZE + DISPATCH_ENTRIES * DISPATCH_ENTRY_SIZE) as u32
    }

    pub fn stop_flag(&self) -> &'static std::sync::atomic::AtomicU32 {
        // SAFETY: inside the region, 4-aligned, lives as long as the (leaked) VM.
        unsafe { &*(self.region.base.add(self.stop_flag_offset() as usize) as *const std::sync::atomic::AtomicU32) }
    }

    /// Linear-memory offset of the dispatch cache (see DISPATCH_ENTRIES).
    pub fn dispatch_cache_offset(&self) -> u32 {
        (self.heap_size + MAX_CLASSES * CLASS_META_SIZE) as u32
    }

    /// Linear-memory offset of class `id`'s metadata.
    pub fn class_meta_offset(&self, id: u32) -> u32 {
        (self.heap_size + id as usize * CLASS_META_SIZE) as u32
    }

    /// Class `id`'s statics cell and initialised flag (see CLASS_META_SIZE).
    pub fn class_meta(&self, id: u32) -> (&'static RefCell32, &'static std::sync::atomic::AtomicBool) {
        assert!((id as usize) < MAX_CLASSES);
        // SAFETY: inside the metadata table, which lives as long as the VM
        // (and the VM is leaked); 4-aligned; zeroed memory is a null Ref and false.
        unsafe {
            let p = self.region.base.add(self.class_meta_offset(id) as usize);
            (&*(p as *const RefCell32), &*(p.add(4) as *const std::sync::atomic::AtomicBool))
        }
    }

    /// A thread stack, or None when all are in use.
    pub fn take_stack(&self) -> Option<ValueStack> {
        let region = self.free_stacks.lock().unwrap().pop()?;
        // The thread block, then the value stack.
        let offset = region + THREAD_BLOCK_SIZE;
        // SAFETY: a stack-sized, 8-aligned range inside the region.
        let base = unsafe { self.region.base.add(offset) } as *mut u64;
        // SAFETY: the start of the region; no other thread uses it.
        let block = unsafe { &*(self.region.base.add(region) as *const ThreadBlock) };
        block.tlab_top.store(0, std::sync::atomic::Ordering::Relaxed);
        block.tlab_end.store(0, std::sync::atomic::Ordering::Relaxed);
        block.native_budget.store(i32::MAX, std::sync::atomic::Ordering::Relaxed);
        Some(ValueStack { base, len: 0, capacity: (STACK_BYTES - THREAD_BLOCK_SIZE) / 8, offset, block })
    }

    pub fn return_stack(&self, stack: &ValueStack) {
        self.free_stacks.lock().unwrap().push(stack.offset - THREAD_BLOCK_SIZE);
    }
}

impl Drop for LinearMemory {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: allocated in `allocate` with this layout.
            unsafe { dealloc(self.region.base, std::alloc::Layout::from_size_align(self.region.size, 4096).unwrap()) }
        }
    }
}

/// An interpreter value stack in linear memory: fixed capacity, slice-like.
pub struct ValueStack {
    base: *mut u64,
    len: usize,
    capacity: usize,
    /// Offset in the region (to return it to the pool).
    offset: usize,
    /// The thread block before it.
    block: &'static ThreadBlock,
}

// SAFETY: owned by one thread; the collector reads it while that thread is stopped.
unsafe impl Send for ValueStack {}

impl ValueStack {
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Whether `additional` more slots fit.
    #[inline]
    pub fn has_room(&self, additional: usize) -> bool {
        self.capacity - self.len >= additional
    }

    /// The thread's block (TLAB) in linear memory.
    pub fn thread_block(&self) -> &'static ThreadBlock {
        self.block
    }

    /// Byte offset of slot 0 in the linear memory.
    #[inline]
    pub fn region_offset(&self) -> usize {
        self.offset
    }

    #[inline]
    pub fn as_ptr(&self) -> *const u64 {
        self.base
    }

    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut u64 {
        self.base
    }

    /// # Safety
    /// `len` must not exceed the capacity; slots up to `len` must be initialised
    /// (the memory is zeroed, so any value is initialised).
    #[inline]
    pub unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.capacity);
        self.len = len;
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }

    /// The frame records sharing this stack's region.
    pub fn frame_stack(&self) -> FrameStack {
        let bytes = self.capacity * 8;
        // SAFETY: the end of the region.
        FrameStack { end: unsafe { (self.base as *mut u8).add(bytes) }, end_offset: self.offset + bytes, len: 0 }
    }

    /// Checked push (rare paths; the interpreter uses unchecked pushes within
    /// a frame's reserved space).
    #[inline]
    pub fn push(&mut self, value: u64) {
        assert!(self.len < self.capacity, "value stack overflow");
        // SAFETY: within capacity.
        unsafe { *self.base.add(self.len) = value };
        self.len += 1;
    }

    #[inline]
    pub fn pop(&mut self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: within the initialised length.
        Some(unsafe { *self.base.add(self.len) })
    }

    /// Grows to `len`, filling new slots with `value`.
    pub fn resize(&mut self, len: usize, value: u64) {
        assert!(len <= self.capacity, "value stack overflow");
        for i in self.len..len {
            // SAFETY: within capacity.
            unsafe { *self.base.add(i) = value };
        }
        self.len = len;
    }

    pub fn extend_from_slice(&mut self, values: &[u64]) {
        assert!(self.has_room(values.len()), "value stack overflow");
        // SAFETY: within capacity; `values` cannot alias the unused part.
        unsafe { std::ptr::copy_nonoverlapping(values.as_ptr(), self.base.add(self.len), values.len()) };
        self.len += values.len();
    }
}

impl std::ops::Deref for ValueStack {
    type Target = [u64];

    fn deref(&self) -> &[u64] {
        // SAFETY: [base, base+len) is initialised and owned by this stack.
        unsafe { std::slice::from_raw_parts(self.base, self.len) }
    }
}

impl std::ops::DerefMut for ValueStack {
    fn deref_mut(&mut self) -> &mut [u64] {
        // SAFETY: as for Deref.
        unsafe { std::slice::from_raw_parts_mut(self.base, self.len) }
    }
}

/// An interpreter frame.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub method: MethodRef,
    /// Bytecode offset of the current instruction (of the pending call, in
    /// callers).
    pub pc: u32,
    /// Index of local 0 in the value stack.
    pub locals: u32,
    /// Pushed by `Thread::invoke` from Rust rather than by a call instruction,
    /// so returning from it must not advance the frame below.
    pub entry: bool,
    /// Monitor held for a synchronized method, released on exit.
    pub monitor: Ref,
}

/// Layout of a frame record in linear memory (compiled code writes them too).
/// RJAVA_JSAMPLE (see `threads::start_sampler`): records that stop being
/// frames are cleared (method 0), so that a sampler can walk the records
/// compiled code pushed above `len` and stop at the first unused one.
pub static SAMPLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[inline]
fn sampling() -> bool {
    SAMPLING.load(std::sync::atomic::Ordering::Relaxed)
}

pub mod frame_layout {
    /// The Method pointer, as a u64.
    pub const METHOD: u32 = 0;
    pub const PC: u32 = 8;
    pub const LOCALS: u32 = 12;
    pub const MONITOR: u32 = 16;
    /// 0 or 1.
    pub const ENTRY: u32 = 20;
    pub const SIZE: usize = 24;
}

/// The frame records of a thread, growing down from the end of its stack
/// region: frame `i` (0 = outermost) is at `end - (i + 1) * SIZE`.
pub struct FrameStack {
    end: *mut u8,
    /// Offset of `end` in the linear memory.
    end_offset: usize,
    len: usize,
}

// SAFETY: as for ValueStack.
unsafe impl Send for FrameStack {}

impl FrameStack {
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes taken by `frames` records.
    #[inline]
    pub fn bytes(frames: usize) -> usize {
        frames * frame_layout::SIZE
    }

    #[inline]
    fn record(&self, index: usize) -> *mut u8 {
        // Records above `len` are valid too: compiled code pushes records
        // for its direct calls without updating `len`.
        // SAFETY: within the thread's stack region (push checks the room).
        unsafe { self.end.sub((index + 1) * frame_layout::SIZE) }
    }

    /// Linear-memory offset of frame `index`'s record.
    #[inline]
    pub fn offset(&self, index: usize) -> u32 {
        (self.end_offset - (index + 1) * frame_layout::SIZE) as u32
    }

    /// The frame whose record is at linear-memory `offset`.
    #[inline]
    pub fn index_at(&self, offset: u32) -> usize {
        (self.end_offset - offset as usize) / frame_layout::SIZE - 1
    }

    #[inline]
    pub fn get(&self, index: usize) -> Frame {
        use frame_layout::*;
        let p = self.record(index);
        // SAFETY: an initialised record; the method pointer was stored from
        // a MethodRef (by `set` or by compiled code).
        unsafe {
            let read = |offset: u32| (p.add(offset as usize) as *const u32).read();
            Frame {
                method: &*((p as *const u64).read() as usize as *const crate::class::Method),
                pc: read(PC),
                locals: read(LOCALS),
                entry: read(ENTRY) != 0,
                monitor: Ref(read(MONITOR)),
            }
        }
    }

    #[inline]
    pub fn set(&mut self, index: usize, frame: Frame) {
        use frame_layout::*;
        let p = self.record(index);
        // SAFETY: within the region, 8-aligned.
        unsafe {
            let write = |offset: u32, v: u32| (p.add(offset as usize) as *mut u32).write(v);
            (p as *mut u64).write(frame.method as *const crate::class::Method as usize as u64);
            write(PC, frame.pc);
            write(LOCALS, frame.locals);
            write(MONITOR, frame.monitor.0);
            write(ENTRY, frame.entry as u32);
        }
    }

    #[inline]
    pub fn top(&self) -> Frame {
        debug_assert!(self.len > 0);
        self.get(self.len - 1)
    }

    #[inline]
    pub fn top_pc(&self) -> u32 {
        // SAFETY: an initialised record.
        unsafe { (self.record(self.len - 1).add(frame_layout::PC as usize) as *const u32).read() }
    }

    #[inline]
    pub fn set_top_pc(&mut self, pc: u32) {
        // SAFETY: as for `top_pc`.
        unsafe { (self.record(self.len - 1).add(frame_layout::PC as usize) as *mut u32).write(pc) }
    }

    /// The caller must have checked the room (see `Thread::push_frame`).
    #[inline]
    pub fn push(&mut self, frame: Frame) {
        self.len += 1;
        self.set(self.len - 1, frame);
    }

    #[inline]
    pub fn pop(&mut self) -> Frame {
        let frame = self.top();
        self.len -= 1;
        if !frame.monitor.is_null() {
            // Released by the caller: compiled code that pushed the record
            // (a synchronized method's `call`) must not release it again.
            // SAFETY: within the region.
            unsafe { (self.record(self.len).add(frame_layout::MONITOR as usize) as *mut u32).write(0) }
        }
        if sampling() {
            self.clear(self.len);
        }
        frame
    }

    /// Adopts records written by compiled code (or drops records).
    #[inline]
    pub fn set_len(&mut self, len: usize) {
        if sampling() {
            for index in len..self.len {
                self.clear(index);
            }
        }
        self.len = len;
    }

    fn clear(&mut self, index: usize) {
        // SAFETY: within the region, 8-aligned.
        unsafe { (self.record(index) as *mut u64).write(0) }
    }

    /// The methods of the frames, outermost first, including those compiled
    /// code pushed above `len` (only meaningful while SAMPLING; racy when
    /// called from another thread, for sampling).
    pub fn sample(&self, out: &mut Vec<MethodRef>) {
        let limit = (STACK_REGION_BYTES / 4) / frame_layout::SIZE;
        for index in 0..limit {
            // SAFETY: within the thread's stack region.
            let method = unsafe { (self.record(index) as *const u64).read_volatile() } as usize;
            if method == 0 {
                if index >= self.len {
                    break;
                }
                continue;
            }
            // SAFETY: records hold Method pointers (methods are never freed).
            out.push(unsafe { &*(method as *const crate::class::Method) });
        }
    }

    /// Outermost first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = Frame> + '_ {
        (0..self.len).map(|i| self.get(i))
    }
}
