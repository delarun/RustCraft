//! jdk.internal.misc.Unsafe.
//!
//! An address is `(base, offset)`: with a non-null base it is a field or
//! element of a heap object; with a null base the offset is an absolute native
//! address (off-heap memory from `allocateMemory`, used heavily by LWJGL).

use super::*;
use crate::heap::ARRAY_BASE_OFFSET;
use std::alloc::{Layout, alloc_zeroed, dealloc, realloc};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

fn address(t: &Thread<'_>, base: Ref, offset: i64) -> *mut u8 {
    if base.is_null() { offset as usize as *mut u8 } else { t.vm.heap.ptr(base, offset as u32) }
}

macro_rules! accessors {
    ($n:ident, $name:literal, $desc:literal, $t:ty, |$v:ident| $to_slot:expr, |$s:ident| $from_slot:expr) => {{
        fn get(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
            // SAFETY: Unsafe's contract: the caller supplies a valid address.
            let $v: $t = unsafe { (address(t, arg_ref(a, 1), arg_j(a, 2)) as *const $t).read_unaligned() };
            Ok(Some($to_slot))
        }
        fn put(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
            let $s = a[4];
            let value: $t = $from_slot;
            // SAFETY: as above.
            unsafe { (address(t, arg_ref(a, 1), arg_j(a, 2)) as *mut $t).write_unaligned(value) };
            ret_void()
        }
        fn get_volatile(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
            let result = get(t, a);
            fence(Ordering::SeqCst);
            result
        }
        fn put_volatile(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
            fence(Ordering::SeqCst);
            let result = put(t, a);
            fence(Ordering::SeqCst);
            result
        }
        $n.add(concat!("jdk/internal/misc/Unsafe.get", $name, "(Ljava/lang/Object;J)", $desc), get);
        $n.add(concat!("jdk/internal/misc/Unsafe.put", $name, "(Ljava/lang/Object;J", $desc, ")V"), put);
        $n.add(concat!("jdk/internal/misc/Unsafe.get", $name, "Volatile(Ljava/lang/Object;J)", $desc), get_volatile);
        $n.add(concat!("jdk/internal/misc/Unsafe.put", $name, "Volatile(Ljava/lang/Object;J", $desc, ")V"), put_volatile);
    }};
}

pub(super) fn register(n: &mut Natives) {
    n.add("jdk/internal/misc/Unsafe.registerNatives()V", no_op);

    accessors!(n, "Int", "I", i32, |v| v as u32 as u64, |s| s as i32);
    accessors!(n, "Long", "J", i64, |v| v as u64, |s| s as i64);
    accessors!(n, "Boolean", "Z", u8, |v| (v != 0) as u64, |s| (s as u8 != 0) as u8);
    accessors!(n, "Byte", "B", i8, |v| v as i32 as u32 as u64, |s| s as i8);
    accessors!(n, "Short", "S", i16, |v| v as i32 as u32 as u64, |s| s as i16);
    accessors!(n, "Char", "C", u16, |v| v as u64, |s| s as u16);
    accessors!(n, "Float", "F", u32, |v| v as u64, |s| s as u32);
    accessors!(n, "Double", "D", u64, |v| v, |s| s);
    accessors!(n, "Reference", "Ljava/lang/Object;", u32, |v| v as u64, |s| s as u32);

    n.add("jdk/internal/misc/Unsafe.compareAndSetInt(Ljava/lang/Object;JII)Z", |t, a| {
        let cell = atomic32(t, a);
        ret_z(cell.compare_exchange(a[4] as u32, a[5] as u32, Ordering::SeqCst, Ordering::SeqCst).is_ok())
    });
    n.add("jdk/internal/misc/Unsafe.compareAndExchangeInt(Ljava/lang/Object;JII)I", |t, a| {
        let cell = atomic32(t, a);
        let witness = cell.compare_exchange(a[4] as u32, a[5] as u32, Ordering::SeqCst, Ordering::SeqCst);
        ret_i(witness.unwrap_or_else(|w| w) as i32)
    });
    n.add("jdk/internal/misc/Unsafe.compareAndSetReference(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Z", |t, a| {
        let cell = atomic32(t, a);
        ret_z(cell.compare_exchange(a[4] as u32, a[5] as u32, Ordering::SeqCst, Ordering::SeqCst).is_ok())
    });
    n.add(
        "jdk/internal/misc/Unsafe.compareAndExchangeReference(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        |t, a| {
            let cell = atomic32(t, a);
            let witness = cell.compare_exchange(a[4] as u32, a[5] as u32, Ordering::SeqCst, Ordering::SeqCst);
            ret_ref(Ref(witness.unwrap_or_else(|w| w)))
        },
    );
    n.add("jdk/internal/misc/Unsafe.compareAndSetLong(Ljava/lang/Object;JJJ)Z", |t, a| {
        let cell = atomic64(t, a);
        ret_z(cell.compare_exchange(a[4], a[6], Ordering::SeqCst, Ordering::SeqCst).is_ok())
    });
    n.add("jdk/internal/misc/Unsafe.compareAndExchangeLong(Ljava/lang/Object;JJJ)J", |t, a| {
        let cell = atomic64(t, a);
        let witness = cell.compare_exchange(a[4], a[6], Ordering::SeqCst, Ordering::SeqCst);
        ret_j(witness.unwrap_or_else(|w| w) as i64)
    });

    n.add("jdk/internal/misc/Unsafe.fullFence()V", |_, _| {
        fence(Ordering::SeqCst);
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.park(ZJ)V", |t, a| {
        t.park(arg_z(a, 1), arg_j(a, 2));
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.unpark(Ljava/lang/Object;)V", |t, a| {
        let thread = arg_ref(a, 1);
        if !thread.is_null() {
            let id = t.field(thread, "java/lang/Thread", "eetop");
            if let Some(target) = t.vm.threads.get(id) {
                target.unpark();
            }
        }
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.getLoadAverage0([DI)I", |_, _| ret_i(-1));

    n.add("jdk/internal/misc/Unsafe.allocateMemory0(J)J", |_, a| ret_j(allocate(arg_j(a, 1) as usize) as i64));
    n.add("jdk/internal/misc/Unsafe.reallocateMemory0(JJ)J", |_, a| {
        ret_j(reallocate(arg_j(a, 1) as usize as *mut u8, arg_j(a, 3) as usize) as i64)
    });
    n.add("jdk/internal/misc/Unsafe.freeMemory0(J)V", |_, a| {
        free(arg_j(a, 1) as usize as *mut u8);
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.setMemory0(Ljava/lang/Object;JJB)V", |t, a| {
        let ptr = address(t, arg_ref(a, 1), arg_j(a, 2));
        // SAFETY: Unsafe contract.
        unsafe { std::ptr::write_bytes(ptr, a[6] as u8, arg_j(a, 4) as usize) };
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.copyMemory0(Ljava/lang/Object;JLjava/lang/Object;JJ)V", |t, a| {
        let src = address(t, arg_ref(a, 1), arg_j(a, 2));
        let dst = address(t, arg_ref(a, 4), arg_j(a, 5));
        // SAFETY: Unsafe contract; ranges may overlap.
        unsafe { std::ptr::copy(src, dst, arg_j(a, 7) as usize) };
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.copySwapMemory0(Ljava/lang/Object;JLjava/lang/Object;JJJ)V", unsafe_copy_swap_memory);

    n.add("jdk/internal/misc/Unsafe.objectFieldOffset1(Ljava/lang/Class;Ljava/lang/String;)J", unsafe_object_field_offset1);
    n.add("jdk/internal/misc/Unsafe.objectFieldOffset0(Ljava/lang/reflect/Field;)J", |t, a| {
        ret_j(reflected_field(t, arg_ref(a, 1))?.1 as i64)
    });
    n.add("jdk/internal/misc/Unsafe.staticFieldOffset0(Ljava/lang/reflect/Field;)J", |t, a| {
        ret_j(reflected_field(t, arg_ref(a, 1))?.1 as i64)
    });
    n.add("jdk/internal/misc/Unsafe.staticFieldBase0(Ljava/lang/reflect/Field;)Ljava/lang/Object;", |t, a| {
        ret_ref(reflected_field(t, arg_ref(a, 1))?.0.statics.get())
    });
    n.add("jdk/internal/misc/Unsafe.arrayBaseOffset0(Ljava/lang/Class;)I", |_, _| ret_i(ARRAY_BASE_OFFSET as i32));
    n.add("jdk/internal/misc/Unsafe.arrayIndexScale0(Ljava/lang/Class;)I", |t, a| {
        let class = t.vm.class_from_mirror(t.null_check(arg_ref(a, 1))?);
        match class.element_kind() {
            Some(kind) => ret_i(kind.size() as i32),
            None => Err(t.throw_new("java/lang/IllegalArgumentException", Some("not an array class"))),
        }
    });
    n.add("jdk/internal/misc/Unsafe.shouldBeInitialized0(Ljava/lang/Class;)Z", |t, a| {
        let class = t.vm.class_from_mirror(t.null_check(arg_ref(a, 1))?);
        ret_z(!t.vm.is_initialized(class))
    });
    n.add("jdk/internal/misc/Unsafe.ensureClassInitialized0(Ljava/lang/Class;)V", |t, a| {
        let class = t.vm.class_from_mirror(t.null_check(arg_ref(a, 1))?);
        t.ensure_initialized(class)?;
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.allocateInstance(Ljava/lang/Class;)Ljava/lang/Object;", |t, a| {
        let class = t.vm.class_from_mirror(t.null_check(arg_ref(a, 1))?);
        if !class.kind.is_instance() || class.is_interface() || class.access_flags & classfile::access::ABSTRACT != 0 {
            return Err(t.throw_new("java/lang/InstantiationException", Some(&class.java_name())));
        }
        t.ensure_initialized(class)?;
        let obj = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
        ret_ref(obj)
    });
    n.add("jdk/internal/misc/Unsafe.throwException(Ljava/lang/Throwable;)V", |t, a| {
        let exception = t.null_check(arg_ref(a, 1))?;
        Err(crate::interp::Throw(exception))
    });
}

fn atomic32<'a>(t: &'a Thread<'_>, a: &[u64]) -> &'a AtomicU32 {
    // SAFETY: CAS targets are naturally aligned int/reference fields.
    unsafe { AtomicU32::from_ptr(address(t, arg_ref(a, 1), arg_j(a, 2)) as *mut u32) }
}

fn atomic64<'a>(t: &'a Thread<'_>, a: &[u64]) -> &'a AtomicU64 {
    // SAFETY: CAS targets are naturally aligned long fields.
    unsafe { AtomicU64::from_ptr(address(t, arg_ref(a, 1), arg_j(a, 2)) as *mut u64) }
}

fn unsafe_copy_swap_memory(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let src = address(t, arg_ref(a, 1), arg_j(a, 2));
    let dst = address(t, arg_ref(a, 4), arg_j(a, 5));
    let (bytes, element) = (arg_j(a, 7) as usize, arg_j(a, 9) as usize);
    // SAFETY: Unsafe contract; non-overlapping per the Java-side checks.
    unsafe {
        let src = std::slice::from_raw_parts(src, bytes).to_vec();
        let dst = std::slice::from_raw_parts_mut(dst, bytes);
        for (d, s) in dst.chunks_exact_mut(element).zip(src.chunks_exact(element)) {
            for (i, byte) in s.iter().rev().enumerate() {
                d[i] = *byte;
            }
        }
    }
    ret_void()
}

fn unsafe_object_field_offset1(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = t.vm.class_from_mirror(t.null_check(arg_ref(a, 1))?);
    let name = t.vm.string_value(t.null_check(arg_ref(a, 2))?);
    match class.fields.iter().find(|f| f.name == name && !f.injected) {
        Some(field) => ret_j(field.offset as i64),
        None => Err(t.throw_new("java/lang/InternalError", Some(&name))),
    }
}

/// The class and offset behind a `java.lang.reflect.Field` (its `slot` indexes
/// the declaring class's fields).
fn reflected_field(t: &mut Thread<'_>, field: Ref) -> JResult<(ClassRef, u32)> {
    let field = t.null_check(field)?;
    let class = t.vm.class_from_mirror(t.field_ref(field, "java/lang/reflect/Field", "clazz"));
    let slot = t.field(field, "java/lang/reflect/Field", "slot") as usize;
    Ok((class, class.fields[slot].offset))
}

// Off-heap memory: a 16-byte header records the size for free/realloc.
const MALLOC_HEADER: usize = 16;

fn allocate(bytes: usize) -> *mut u8 {
    let Ok(layout) = Layout::from_size_align(bytes + MALLOC_HEADER, MALLOC_HEADER) else { return std::ptr::null_mut() };
    // SAFETY: non-zero layout.
    unsafe {
        let base = alloc_zeroed(layout);
        if base.is_null() {
            return base;
        }
        (base as *mut usize).write(bytes);
        base.add(MALLOC_HEADER)
    }
}

fn free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` came from `allocate`.
    unsafe {
        let base = ptr.sub(MALLOC_HEADER);
        let bytes = (base as *const usize).read();
        dealloc(base, Layout::from_size_align_unchecked(bytes + MALLOC_HEADER, MALLOC_HEADER));
    }
}

fn reallocate(ptr: *mut u8, bytes: usize) -> *mut u8 {
    if ptr.is_null() {
        return allocate(bytes);
    }
    // SAFETY: `ptr` came from `allocate`.
    unsafe {
        let base = ptr.sub(MALLOC_HEADER);
        let old = (base as *const usize).read();
        let new_base = realloc(base, Layout::from_size_align_unchecked(old + MALLOC_HEADER, MALLOC_HEADER), bytes + MALLOC_HEADER);
        if new_base.is_null() {
            return new_base;
        }
        (new_base as *mut usize).write(bytes);
        new_base.add(MALLOC_HEADER)
    }
}

pub(super) fn set_unsafe_constants(t: &mut Thread<'_>, class: ClassRef) {
    t.set_static(class, "ADDRESS_SIZE0", std::mem::size_of::<usize>() as u64);
    t.set_static(class, "PAGE_SIZE", 4096);
    t.set_static(class, "BIG_ENDIAN", cfg!(target_endian = "big") as u64);
    t.set_static(class, "UNALIGNED_ACCESS", 1);
    t.set_static(class, "DATA_CACHE_LINE_FLUSH_SIZE", 0);
}
