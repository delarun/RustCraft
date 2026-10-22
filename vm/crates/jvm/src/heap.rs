//! The Java heap: one contiguous region addressed by 32-bit offsets.
//!
//! Using offsets instead of native pointers keeps references 4 bytes on every
//! target and maps directly onto a WASM linear-memory region, which JIT-compiled
//! code and native (LWJGL) modules will share.
//!
//! Object layout:
//! ```text
//! 0  u32 class id        (index into the VM class table)
//! 4  u32 mark word       (identity hash; lock bits later)
//! 8  instance fields...  (arrays: u32 length at 8, elements from 16)
//! ```
//!
//! Allocation uses free lists left by a non-moving mark-sweep collector (see
//! `gc.rs`), falling back to a bump pointer. Object starts are recorded in a
//! bitmap, which lets the collector recognise references conservatively.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub const HEADER_SIZE: u32 = 8;
pub const ARRAY_LENGTH_OFFSET: u32 = 8;
pub const ARRAY_BASE_OFFSET: u32 = 16;
const ALIGN: usize = 8;
/// Smallest free-list chunk taken for an allocation buffer while the bump
/// region has room (RJAVA_TLAB_MIN overrides).
const TLAB_MIN_FREE: usize = 0;

/// A reference to a heap object; 0 is `null`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Ref(pub u32);

impl Ref {
    pub const NULL: Ref = Ref(0);

    pub fn is_null(self) -> bool {
        self.0 == 0
    }

    pub fn from_slot(slot: u64) -> Ref {
        Ref(slot as u32)
    }

    pub fn slot(self) -> u64 {
        self.0 as u64
    }
}

impl std::fmt::Debug for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_null() { f.write_str("null") } else { write!(f, "@{:#x}", self.0) }
    }
}

/// A heap-resident reference that the runtime stores outside the heap (class
/// mirrors, statics blocks, loaders). Atomic so it can be updated by a future
/// moving collector and read from other threads.
#[derive(Default)]
#[repr(transparent)]
pub struct RefCell32(AtomicU32);

impl RefCell32 {
    /// Stores `r` if the cell is null; returns the cell's value afterwards.
    pub fn publish(&self, r: Ref) -> Ref {
        match self.0.compare_exchange(0, r.0, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => r,
            Err(existing) => Ref(existing),
        }
    }

    pub fn new(r: Ref) -> Self {
        RefCell32(AtomicU32::new(r.0))
    }

    pub fn get(&self) -> Ref {
        Ref(self.0.load(Ordering::Acquire))
    }

    pub fn set(&self, r: Ref) {
        self.0.store(r.0, Ordering::Release)
    }
}

impl std::fmt::Debug for RefCell32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.get().fmt(f)
    }
}

pub struct Heap {
    base: *mut u8,
    capacity: usize,
    /// Allocated by `Heap::new` (freed on drop) rather than borrowed from a region.
    owned: bool,
    /// Bump pointer; memory above it has never been allocated.
    top: AtomicUsize,
    /// One bit per 8-byte granule: an object starts here.
    starts: Bitmap,
    /// One bit per granule: marked live by the current collection.
    marks: Bitmap,
    /// One bit per granule: live object that must not move (see `gc.rs`).
    pins: Bitmap,
    /// Free chunks left by the last sweep.
    free: Mutex<FreeLists>,
    /// Bytes allocated since the last collection.
    allocated_since_gc: AtomicUsize,
    /// Live bytes found by the last collection.
    live_after_gc: AtomicUsize,
    /// Allocation volume that triggers the next collection.
    gc_threshold: AtomicUsize,
    /// The bump pointer and the largest free chunk after the last compaction.
    compacted_top: AtomicUsize,
    compacted_largest: AtomicUsize,
    /// Compact at the next collection whatever `should_compact` says (an
    /// allocation failed after a collection).
    force_compaction: std::sync::atomic::AtomicBool,
}

/// Size-segregated free chunks (offsets): exact bins for small chunks, a
/// size-ordered map for large ones.
#[derive(Default)]
struct FreeLists {
    small: Vec<Vec<u32>>,
    large: BTreeMap<u32, Vec<u32>>,
    bytes: usize,
}

const SMALL_GRANULES: usize = 64;

impl FreeLists {
    fn push(&mut self, offset: u32, granules: u32) {
        self.bytes += granules as usize * ALIGN;
        if (granules as usize) <= SMALL_GRANULES {
            if self.small.len() <= SMALL_GRANULES {
                self.small.resize_with(SMALL_GRANULES + 1, Vec::new);
            }
            self.small[granules as usize].push(offset);
        } else {
            self.large.entry(granules).or_default().push(offset);
        }
    }

    /// Some free chunk of `min..=max` granules (a larger one is split), so that
    /// allocation buffers fill fragmented space before using the bump region.
    fn take_between(&mut self, min: u32, max: u32) -> Option<(u32, u32)> {
        // Prefer a chunk at least `max` big (split it), else the largest smaller one.
        let found = match self.large.range(max..).next() {
            Some((&size, _)) => Some(size),
            None => self.large.range(min.max(SMALL_GRANULES as u32 + 1)..max).next_back().map(|(&size, _)| size),
        };
        if let Some(size) = found {
            let list = self.large.get_mut(&size).unwrap();
            let offset = list.pop().unwrap();
            if list.is_empty() {
                self.large.remove(&size);
            }
            self.bytes -= size as usize * ALIGN;
            let take = size.min(max);
            if size > take {
                self.push(offset + take * ALIGN as u32, size - take);
            }
            return Some((offset, take));
        }
        for size in (min as usize..=SMALL_GRANULES.min(max as usize)).rev() {
            if let Some(offset) = self.small.get_mut(size).and_then(Vec::pop) {
                self.bytes -= size * ALIGN;
                return Some((offset, size as u32));
            }
        }
        None
    }

    /// A chunk of exactly `granules`, splitting a larger one if needed.
    fn take(&mut self, granules: u32) -> Option<u32> {
        let g = granules as usize;
        let mut found = None;
        if g <= SMALL_GRANULES {
            for size in g..=SMALL_GRANULES {
                if let Some(offset) = self.small.get_mut(size).and_then(Vec::pop) {
                    found = Some((offset, size as u32));
                    break;
                }
            }
        }
        if found.is_none() {
            let size = *self.large.range(granules..).next()?.0;
            let list = self.large.get_mut(&size).unwrap();
            let offset = list.pop().unwrap();
            if list.is_empty() {
                self.large.remove(&size);
            }
            found = Some((offset, size));
        }
        let (offset, size) = found?;
        self.bytes -= size as usize * ALIGN;
        if size > granules {
            self.push(offset + granules * ALIGN as u32, size - granules);
        }
        Some(offset)
    }
}

/// Where each live object goes in a compaction.
pub struct Compaction {
    /// (address, size) of every live object, in address order.
    objects: Vec<(u32, u32)>,
    /// New address of each object.
    forward: Vec<u32>,
    /// For each block of COMPACTION_BLOCK bytes (and one past the last), the
    /// index of the first object at or after its start.
    blocks: Vec<u32>,
}

const COMPACTION_BLOCK: usize = 512;

impl Compaction {
    /// The new location of a live object (other references are unchanged).
    pub fn forward(&self, r: Ref) -> Ref {
        let block = r.0 as usize / COMPACTION_BLOCK;
        if block + 1 >= self.blocks.len() {
            return r;
        }
        let (from, to) = (self.blocks[block] as usize, self.blocks[block + 1] as usize);
        match self.objects[from..to].binary_search_by_key(&r.0, |&(addr, _)| addr) {
            Ok(index) => Ref(self.forward[from + index]),
            Err(_) => r,
        }
    }

    pub fn objects(&self) -> &[(u32, u32)] {
        &self.objects
    }

    pub fn moved(&self) -> usize {
        self.objects.iter().zip(&self.forward).filter(|((addr, _), target)| addr != *target).count()
    }
}

/// An atomic bitmap over heap granules.
struct Bitmap {
    /// Leaked with the heap (heaps live as long as their VM), or in the
    /// VM's linear memory (the starts bitmap, which compiled code updates).
    words: &'static [AtomicU64],
}

impl Bitmap {
    fn new(granules: usize) -> Bitmap {
        let words = granules.div_ceil(64);
        Bitmap { words: Box::leak((0..words).map(|_| AtomicU64::new(0)).collect()) }
    }

    /// Bytes of a bitmap for `granules`.
    pub fn bytes(granules: usize) -> usize {
        granules.div_ceil(64) * 8
    }

    #[inline]
    fn get(&self, granule: usize) -> bool {
        self.words[granule / 64].load(Ordering::Relaxed) & (1 << (granule % 64)) != 0
    }

    #[inline]
    fn set(&self, granule: usize) {
        self.words[granule / 64].fetch_or(1 << (granule % 64), Ordering::Relaxed);
    }

    #[inline]
    fn clear(&self, granule: usize) {
        self.words[granule / 64].fetch_and(!(1 << (granule % 64)), Ordering::Relaxed);
    }

    /// Sets the bit, returning whether it was clear before.
    #[inline]
    fn test_and_set(&self, granule: usize) -> bool {
        let bit = 1 << (granule % 64);
        self.words[granule / 64].fetch_or(bit, Ordering::Relaxed) & bit == 0
    }

    /// The next set bit at or after `from`, below `limit`.
    fn next_set(&self, from: usize, limit: usize) -> Option<usize> {
        let mut index = from / 64;
        let mut word = self.words.get(index)?.load(Ordering::Relaxed) & (!0u64 << (from % 64));
        loop {
            if word != 0 {
                let bit = index * 64 + word.trailing_zeros() as usize;
                return (bit < limit).then_some(bit);
            }
            index += 1;
            if index * 64 >= limit {
                return None;
            }
            word = self.words[index].load(Ordering::Relaxed);
        }
    }
}

// SAFETY: the heap is plain memory; concurrent access discipline is the Java
// memory model's concern, as in any JVM. Allocation and collection happen
// under the GIL.
unsafe impl Send for Heap {}
unsafe impl Sync for Heap {}

/// Values that can be stored in heap memory.
pub trait HeapValue: Copy {}
impl HeapValue for u8 {}
impl HeapValue for i8 {}
impl HeapValue for u16 {}
impl HeapValue for i16 {}
impl HeapValue for u32 {}
impl HeapValue for i32 {}
impl HeapValue for u64 {}
impl HeapValue for i64 {}
impl HeapValue for f32 {}
impl HeapValue for f64 {}

/// Minimum allocation volume between collections.
const MIN_GC_THRESHOLD: usize = 32 << 20;

impl Heap {
    /// Reserves `capacity` bytes (at most 4 GiB). The memory is zero-initialised
    /// lazily by the OS, so a large reservation is cheap.
    pub fn new(capacity: usize) -> Heap {
        let layout = Layout::from_size_align(capacity, ALIGN).expect("heap layout");
        // SAFETY: non-zero size layout.
        let base = unsafe { alloc_zeroed(layout) };
        assert!(!base.is_null(), "failed to reserve {capacity} bytes of heap");
        let mut heap = Self::in_region(base, capacity, None);
        heap.owned = true;
        heap
    }

    /// A heap over zeroed memory owned by someone else (the VM's linear
    /// memory), optionally with the object-start bitmap at `starts`
    /// (zeroed, `starts_bytes(capacity)` long).
    pub fn in_region(base: *mut u8, capacity: usize, starts: Option<*mut u8>) -> Heap {
        assert!(capacity as u64 <= u32::MAX as u64 + 1, "heap offsets are 32-bit");
        let granules = capacity / ALIGN;
        let starts = match starts {
            // SAFETY: as documented, zeroed and long enough; 8-aligned.
            Some(p) => Bitmap { words: unsafe { std::slice::from_raw_parts(p as *const AtomicU64, granules.div_ceil(64)) } },
            None => Bitmap::new(granules),
        };
        Heap {
            base,
            capacity,
            owned: false,
            // Offset 0 is null; start allocating after it.
            top: AtomicUsize::new(ALIGN),
            starts,
            marks: Bitmap::new(granules),
            pins: Bitmap::new(granules),
            free: Mutex::new(FreeLists::default()),
            allocated_since_gc: AtomicUsize::new(0),
            live_after_gc: AtomicUsize::new(0),
            gc_threshold: AtomicUsize::new(MIN_GC_THRESHOLD.max(capacity / 8)),
            compacted_top: AtomicUsize::new(0),
            compacted_largest: AtomicUsize::new(usize::MAX),
            force_compaction: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Bytes of the object-start bitmap of a heap of `capacity` bytes: one
    /// bit per 8-byte granule (bit `offset / 8`).
    pub fn starts_bytes(capacity: usize) -> usize {
        Bitmap::bytes(capacity / ALIGN)
    }

    /// Bytes in use: live after the last collection plus allocated since.
    pub fn used(&self) -> usize {
        self.live_after_gc.load(Ordering::Relaxed) + self.allocated_since_gc.load(Ordering::Relaxed)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn base(&self) -> *mut u8 {
        self.base
    }

    /// Whether enough has been allocated to warrant a collection.
    #[inline]
    pub fn should_collect(&self) -> bool {
        self.allocated_since_gc.load(Ordering::Relaxed) >= self.gc_threshold.load(Ordering::Relaxed)
    }

    /// Allocates zeroed memory, or `None` when no space is left.
    pub fn alloc(&self, size: u32) -> Option<Ref> {
        let size = (size as usize).max(ALIGN).next_multiple_of(ALIGN);
        let granules = size / ALIGN;
        let from_free = self.free.lock().unwrap().take(granules as u32);
        let offset = match from_free {
            Some(offset) => {
                // SAFETY: a free chunk inside the heap.
                unsafe { std::ptr::write_bytes(self.base.add(offset as usize), 0, size) };
                offset as usize
            }
            None => {
                let mut top = self.top.load(Ordering::Relaxed);
                loop {
                    let end = top.checked_add(size).filter(|&end| end <= self.capacity)?;
                    match self.top.compare_exchange_weak(top, end, Ordering::Relaxed, Ordering::Relaxed) {
                        Ok(_) => break top,
                        Err(current) => top = current,
                    }
                }
            }
        };
        self.starts.set(offset / ALIGN);
        self.allocated_since_gc.fetch_add(size, Ordering::Relaxed);
        Some(Ref(offset as u32))
    }

    /// A zeroed chunk of exactly `size` bytes for a thread-local allocation
    /// buffer, or None if the heap has no such space left. The chunk is not
    /// an object; objects carved out of it are registered with `set_start`.
    /// A zeroed chunk for a thread-local allocation buffer: at least `min`
    /// and at most `max` bytes. Free-list fragments are used first; the bump
    /// region (kept for large objects) only when none fits.
    pub fn alloc_chunk(&self, min: usize, max: usize) -> Option<(u32, u32)> {
        // Free-list chunks of at least TLAB_MIN_FREE, else the bump region,
        // else any chunk that fits: a buffer from a small hole would have to
        // be refilled after a few objects.
        static MIN_FREE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let min_free = *MIN_FREE.get_or_init(|| std::env::var("RJAVA_TLAB_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(TLAB_MIN_FREE));
        let big = (min.max(min_free).min(max) / ALIGN) as u32;
        let mut from_free = self.free.lock().unwrap().take_between(big, (max / ALIGN) as u32);
        if from_free.is_none() && self.capacity - self.top.load(Ordering::Relaxed) < max {
            from_free = self.free.lock().unwrap().take_between((min / ALIGN) as u32, (max / ALIGN) as u32);
        }
        let (offset, size) = match from_free {
            Some((offset, granules)) => {
                let size = granules as usize * ALIGN;
                // SAFETY: a free chunk inside the heap.
                unsafe { std::ptr::write_bytes(self.base.add(offset as usize), 0, size) };
                (offset as usize, size)
            }
            None => {
                let mut top = self.top.load(Ordering::Relaxed);
                loop {
                    let size = if top + max <= self.capacity { max } else { min };
                    let end = top.checked_add(size).filter(|&end| end <= self.capacity)?;
                    match self.top.compare_exchange_weak(top, end, Ordering::Relaxed, Ordering::Relaxed) {
                        Ok(_) => break (top, size),
                        Err(current) => top = current,
                    }
                }
            }
        };
        self.allocated_since_gc.fetch_add(size, Ordering::Relaxed);
        Some((offset as u32, (offset + size) as u32))
    }

    /// Records an object start inside a thread-local buffer (atomically: a
    /// bitmap word can cover two threads' buffers).
    #[inline]
    pub fn set_start(&self, r: Ref) {
        self.starts.set(r.0 as usize / ALIGN);
    }

    /// Whether `r` is the start of an allocated object (for conservative roots).
    #[inline]
    pub fn is_object(&self, r: Ref) -> bool {
        let offset = r.0 as usize;
        offset != 0 && offset % ALIGN == 0 && offset < self.top.load(Ordering::Relaxed) && self.starts.get(offset / ALIGN)
    }

    /// Marks an object; returns true if it was not marked yet.
    #[inline]
    pub fn mark(&self, r: Ref) -> bool {
        self.marks.test_and_set(r.0 as usize / ALIGN)
    }

    #[inline]
    pub fn pin(&self, r: Ref) {
        self.pins.set(r.0 as usize / ALIGN);
    }

    /// Whether free space is too fragmented: very little never-used bump
    /// region remains, or the free lists have splintered into many tiny
    /// pieces. (Pinned objects can keep the top high, so a low bump region
    /// alone does not mean compaction would help.)
    /// Compaction is slow (everything live moves): only when contiguous
    /// space for large objects and TLABs runs low, and the bump region has
    /// shrunk noticeably since the last compaction (pinned objects can keep
    /// the top high, and compacting again would not lower it).
    /// Also when the largest free chunk shrank to half of what the last
    /// compaction left (pinned objects bound what compaction achieves).
    pub fn should_compact(&self) -> bool {
        if self.force_compaction.swap(false, Ordering::Relaxed) {
            return true;
        }
        let top = self.top.load(Ordering::Relaxed);
        if self.capacity - top >= self.capacity / 8 {
            return false;
        }
        let grown = top - top.min(self.compacted_top.load(Ordering::Relaxed)) > self.capacity / 32;
        let largest = self.free.lock().unwrap().large.keys().next_back().map_or(0, |&g| g as usize * ALIGN);
        grown || largest < (self.capacity / 64).min(self.compacted_largest.load(Ordering::Relaxed) / 2)
    }

    /// Makes the next collection compact.
    pub fn force_compaction(&self) {
        self.force_compaction.store(true, Ordering::Relaxed);
    }

    /// Bytes to allocate before the next collection: at least as much as is
    /// live (the heap doubles), or a third of the free space if more, so a
    /// big heap is collected less often.
    fn next_threshold(&self, live: usize) -> usize {
        let free = self.capacity.saturating_sub(live);
        live.max(free / 3).max(MIN_GC_THRESHOLD).min(self.capacity / 2)
    }

    /// Plans a sliding compaction of the marked objects: each moves down to
    /// the lowest free address, except pinned objects, which stay in place.
    pub fn plan_compaction(&self, size_of: impl Fn(Ref) -> u32) -> Compaction {
        let limit = self.top.load(Ordering::Relaxed) / ALIGN;
        let mut objects = Vec::new();
        let mut granule = 1;
        while let Some(start) = self.starts.next_set(granule, limit) {
            let obj = Ref((start * ALIGN) as u32);
            if self.marks.get(start) {
                let size = (size_of(obj) as usize).max(ALIGN).next_multiple_of(ALIGN) as u32;
                objects.push((obj.0, size));
                granule = start + size as usize / ALIGN;
            } else {
                granule = start + 1;
            }
        }
        let mut blocks = Vec::with_capacity(limit * ALIGN / COMPACTION_BLOCK + 2);
        for (index, &(addr, _)) in objects.iter().enumerate() {
            while blocks.len() <= addr as usize / COMPACTION_BLOCK {
                blocks.push(index as u32);
            }
        }
        blocks.push(objects.len() as u32);
        let mut forward = Vec::with_capacity(objects.len());
        let mut free = ALIGN as u32;
        for &(addr, size) in &objects {
            // Pinned objects are never overtaken: `free` is always at or below
            // the next object, so moved objects cannot overlap them.
            let target = if self.pins.get(addr as usize / ALIGN) { addr } else { free };
            forward.push(target);
            free = target + size;
        }
        Compaction { objects, forward, blocks }
    }

    /// Moves objects as planned and rebuilds the bitmaps and free lists.
    /// References must already have been updated with `Compaction::forward`.
    pub fn compact(&self, plan: &Compaction) -> usize {
        let old_top = self.top.load(Ordering::Relaxed);
        for (&(addr, size), &target) in plan.objects.iter().zip(&plan.forward) {
            if addr != target {
                self.copy(Ref(addr), 0, Ref(target), 0, size as usize);
            }
        }
        let words = (old_top / ALIGN).div_ceil(64).min(self.starts.words.len());
        for i in 0..words {
            self.starts.words[i].store(0, Ordering::Relaxed);
            self.marks.words[i].store(0, Ordering::Relaxed);
            self.pins.words[i].store(0, Ordering::Relaxed);
        }
        let mut free = FreeLists::default();
        let mut end = ALIGN;
        let mut live = 0;
        for (&(_, size), &target) in plan.objects.iter().zip(&plan.forward) {
            let target = target as usize;
            if target > end {
                free.push(end as u32, ((target - end) / ALIGN) as u32);
            }
            self.starts.set(target / ALIGN);
            end = target + size as usize;
            live += size as usize;
        }
        if end < old_top {
            // SAFETY: [end, old_top) is inside the heap and now unused.
            unsafe { std::ptr::write_bytes(self.base.add(end), 0, old_top - end) };
        }
        self.top.store(end, Ordering::Relaxed);
        self.compacted_top.store(end, Ordering::Relaxed);
        self.compacted_largest.store(free.large.keys().next_back().map_or(0, |&g| g as usize * ALIGN), Ordering::Relaxed);
        *self.free.lock().unwrap() = free;
        self.live_after_gc.store(live, Ordering::Relaxed);
        self.allocated_since_gc.store(0, Ordering::Relaxed);
        self.gc_threshold.store(self.next_threshold(live), Ordering::Relaxed);
        live
    }

    #[inline]
    pub fn is_marked(&self, r: Ref) -> bool {
        self.marks.get(r.0 as usize / ALIGN)
    }

    /// Frees every unmarked object and clears the marks. `size_of` gives the
    /// size of a (live) object. Returns the live byte count.
    pub fn sweep(&self, size_of: impl Fn(Ref) -> u32 + Sync) -> usize {
        // RJAVA_GC_VERIFY: fill freed memory with a pattern so that a missed
        // root shows up as an invalid class id instead of silent corruption.
        let poison = std::env::var_os("RJAVA_GC_VERIFY").is_some();
        let top = self.top.load(Ordering::Relaxed);
        let limit = top / ALIGN;
        // Ranges of granules (bitmap-word aligned), swept in parallel; an
        // object belongs to the range it starts in.
        let threads = crate::processors().min(8);
        let parts = threads.min(limit / (1 << 16) + 1);
        let step = (limit / parts).next_multiple_of(64).max(64);
        let bounds: Vec<(usize, usize)> = (0..parts).map(|i| ((i * step).max(1), ((i + 1) * step).min(limit))).filter(|(a, b)| a < b).collect();
        struct Part {
            first: Option<usize>,
            end: usize,
            gaps: Vec<(usize, usize)>,
            live: usize,
        }
        let sweep_part = |(from, to): (usize, usize)| {
            let mut part = Part { first: None, end: 0, gaps: Vec::new(), live: 0 };
            let mut granule = from;
            while let Some(start) = self.starts.next_set(granule, to) {
                if self.marks.get(start) {
                    self.marks.clear(start);
                    self.pins.clear(start);
                    let size = (size_of(Ref((start * ALIGN) as u32)) as usize).max(ALIGN).next_multiple_of(ALIGN);
                    let offset = start * ALIGN;
                    match part.first {
                        None => part.first = Some(offset),
                        Some(_) if offset > part.end => part.gaps.push((part.end, offset - part.end)),
                        Some(_) => {}
                    }
                    part.live += size;
                    part.end = offset + size;
                    granule = part.end / ALIGN;
                } else {
                    self.starts.clear(start);
                    granule = start + 1;
                }
            }
            part
        };
        let results: Vec<Part> = if bounds.len() <= 1 {
            bounds.iter().map(|&b| sweep_part(b)).collect()
        } else {
            let results: Vec<Mutex<Option<Part>>> = bounds.iter().map(|_| Mutex::new(None)).collect();
            crate::pool::run(bounds.len(), |i| *results[i].lock().unwrap() = Some(sweep_part(bounds[i])));
            results.into_iter().map(|r| r.into_inner().unwrap().unwrap()).collect()
        };
        let mut free = FreeLists::default();
        let mut live = 0usize;
        let mut gap_start = ALIGN;
        let mut add_gap = |start: usize, len: usize| {
            free.push(start as u32, (len / ALIGN) as u32);
            if poison {
                // SAFETY: a free gap inside the heap.
                unsafe { std::ptr::write_bytes(self.base.add(start), 0xA5, len) };
            }
        };
        for part in results {
            let Some(first) = part.first else { continue };
            if first > gap_start {
                add_gap(gap_start, first - gap_start);
            }
            for (start, len) in part.gaps {
                add_gap(start, len);
            }
            live += part.live;
            gap_start = gap_start.max(part.end);
        }
        // Trailing free space returns to the bump region, which must read as
        // zero like never-used memory.
        let new_top = gap_start.max(ALIGN);
        if new_top < top {
            // SAFETY: [new_top, top) lies inside the heap and holds only dead objects.
            unsafe { std::ptr::write_bytes(self.base.add(new_top), 0, top - new_top) };
        }
        self.top.store(new_top, Ordering::Relaxed);
        *self.free.lock().unwrap() = free;
        self.live_after_gc.store(live, Ordering::Relaxed);
        self.allocated_since_gc.store(0, Ordering::Relaxed);
        self.gc_threshold.store(self.next_threshold(live), Ordering::Relaxed);
        live
    }

    /// Visits every allocated object in address order; `f` returns its size.
    pub fn for_each_object(&self, mut f: impl FnMut(Ref) -> u32) {
        let limit = self.top.load(Ordering::Relaxed) / ALIGN;
        let mut granule = 1;
        while let Some(start) = self.starts.next_set(granule, limit) {
            let size = (f(Ref((start * ALIGN) as u32)) as usize).max(ALIGN).next_multiple_of(ALIGN);
            granule = start + size / ALIGN;
        }
    }

    /// (free chunks, largest free chunk in bytes, bump space left) for diagnostics.
    pub fn free_stats(&self) -> (usize, usize, usize) {
        let free = self.free.lock().unwrap();
        let small: usize = free.small.iter().map(Vec::len).sum();
        let large: usize = free.large.values().map(Vec::len).sum();
        let largest = free.large.keys().next_back().map_or_else(
            || free.small.iter().rposition(|l| !l.is_empty()).unwrap_or(0),
            |&g| g as usize,
        ) * ALIGN;
        (small + large, largest, self.capacity - self.top.load(Ordering::Relaxed))
    }

    pub fn free_bytes(&self) -> usize {
        self.free.lock().unwrap().bytes + (self.capacity - self.top.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn ptr(&self, r: Ref, offset: u32) -> *mut u8 {
        debug_assert!(!r.is_null(), "null dereference reached the heap");
        debug_assert!((r.0 as usize + offset as usize) < self.top.load(Ordering::Relaxed));
        // SAFETY: references are offsets of live allocations inside the region.
        unsafe { self.base.add(r.0 as usize + offset as usize) }
    }

    #[inline]
    pub fn read<T: HeapValue>(&self, r: Ref, offset: u32) -> T {
        // SAFETY: field offsets are naturally aligned within 8-aligned objects.
        unsafe { (self.ptr(r, offset) as *const T).read() }
    }

    #[inline]
    pub fn write<T: HeapValue>(&self, r: Ref, offset: u32, value: T) {
        // SAFETY: as for `read`.
        unsafe { (self.ptr(r, offset) as *mut T).write(value) }
    }

    #[inline]
    pub fn atomic_u32(&self, r: Ref, offset: u32) -> &AtomicU32 {
        // SAFETY: 4-aligned field inside a live object.
        unsafe { AtomicU32::from_ptr(self.ptr(r, offset) as *mut u32) }
    }

    #[inline]
    pub fn atomic_u64(&self, r: Ref, offset: u32) -> &std::sync::atomic::AtomicU64 {
        // SAFETY: 8-aligned field inside a live object.
        unsafe { std::sync::atomic::AtomicU64::from_ptr(self.ptr(r, offset) as *mut u64) }
    }

    #[inline]
    pub fn class_id(&self, r: Ref) -> u32 {
        self.read(r, 0)
    }

    #[inline]
    pub fn array_length(&self, r: Ref) -> i32 {
        self.read(r, ARRAY_LENGTH_OFFSET)
    }

    pub fn bytes(&self, r: Ref, offset: u32, len: usize) -> &[u8] {
        if len == 0 {
            return &[];
        }
        // SAFETY: caller passes a range inside one object.
        unsafe { std::slice::from_raw_parts(self.ptr(r, offset), len) }
    }

    #[allow(clippy::mut_from_ref)]
    pub fn bytes_mut(&self, r: Ref, offset: u32, len: usize) -> &mut [u8] {
        if len == 0 {
            return &mut [];
        }
        // SAFETY: as for `bytes`; Java code has no aliasing guarantees to break.
        unsafe { std::slice::from_raw_parts_mut(self.ptr(r, offset), len) }
    }

    /// Copies `len` bytes between (possibly overlapping) heap ranges.
    pub fn copy(&self, src: Ref, src_offset: u32, dst: Ref, dst_offset: u32, len: usize) {
        if len == 0 {
            return;
        }
        // SAFETY: both ranges lie within live objects; `copy` allows overlap.
        unsafe { std::ptr::copy(self.ptr(src, src_offset), self.ptr(dst, dst_offset), len) }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: allocated in `new` with this layout.
            unsafe { dealloc(self.base, Layout::from_size_align(self.capacity, ALIGN).unwrap()) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_aligned_zeroed_memory() {
        let heap = Heap::new(1 << 20);
        let a = heap.alloc(12).unwrap();
        let b = heap.alloc(1).unwrap();
        assert_ne!(a, Ref::NULL);
        assert_eq!(a.0 % 8, 0);
        assert_eq!(b.0 - a.0, 16);
        assert_eq!(heap.read::<u64>(a, 0), 0);
        heap.write::<i32>(a, 8, -5);
        assert_eq!(heap.read::<i32>(a, 8), -5);
        assert!(heap.alloc(1 << 21).is_none());
    }

    #[test]
    fn sweeps_and_reuses_free_space() {
        let heap = Heap::new(1 << 20);
        let objects: Vec<Ref> = (0..10).map(|_| heap.alloc(24).unwrap()).collect();
        for &obj in &objects {
            heap.write::<u64>(obj, 8, 0xdead_beef);
            assert!(heap.is_object(obj));
        }
        // Keep the even objects.
        for obj in objects.iter().step_by(2) {
            assert!(heap.mark(*obj));
            assert!(!heap.mark(*obj), "already marked");
        }
        let live = heap.sweep(|_| 24);
        assert_eq!(live, 5 * 24);
        assert!(!heap.is_object(objects[1]));
        assert!(heap.is_object(objects[0]) && !heap.is_marked(objects[0]));
        // Freed gaps are reused and zeroed; the last dead object went back to
        // the bump region, which must also read as zero when handed out again.
        let tail = objects[9];
        let reused = heap.alloc(24).unwrap();
        assert!(objects[1..9].contains(&reused), "{reused:?}");
        assert_eq!(heap.read::<u64>(reused, 8), 0);
        // Exhaust the gaps (4 × 24 bytes) so the next allocation bumps into the old tail.
        for _ in 0..3 {
            heap.alloc(24).unwrap();
        }
        let bumped = heap.alloc(24).unwrap();
        assert_eq!(bumped, tail);
        assert_eq!(heap.read::<u64>(bumped, 8), 0, "bump memory must be zeroed after the top retreats");
    }
}
