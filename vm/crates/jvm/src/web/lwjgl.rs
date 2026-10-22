//! LWJGL's core natives (liblwjgl): memory access and allocation, the
//! thread's JNIEnv, dynamic linking, libc string functions and parts of the
//! JNI function table.

use super::*;
use crate::native::*;

const LIBC: &str = "org/lwjgl/system/libc/LibCString";

/// The JNI function table LWJGL sees (only its size and reserved slots
/// matter: LWJGL keeps per-thread data there).
const JNI_FUNCTIONS: usize = 240;

thread_local! {
    /// This thread's JNIEnv: a pointer to its function table.
    static ENV: Cell<u32> = const { Cell::new(0) };
}

fn shared_table() -> u32 {
    static TABLE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // SAFETY: a zeroed allocation that lives forever.
    *TABLE.get_or_init(|| unsafe { calloc(JNI_FUNCTIONS, 4) } as u32)
}

fn thread_env() -> u32 {
    ENV.with(|env| {
        if env.get() == 0 {
            // SAFETY: a zeroed allocation for this thread's lifetime.
            let block = unsafe { calloc(16, 4) } as u32;
            unsafe { *(block as usize as *mut u32) = shared_table() };
            env.set(block);
        }
        env.get()
    })
}

extern "C" fn function_missing_abort() {
    panic!("LWJGL called a function the platform does not provide");
}

/// Reads `T` at `address`.
fn get<T: Copy>(address: i64) -> T {
    // SAFETY: LWJGL passes valid addresses.
    unsafe { (address as usize as *const T).read_unaligned() }
}

fn put<T>(address: i64, value: T) {
    // SAFETY: as above.
    unsafe { (address as usize as *mut T).write_unaligned(value) }
}

pub(super) fn register(n: &mut Natives) {
    n.add("org/lwjgl/system/MemoryAccessJNI.getPointerSize()I", |_, _| ret_i(4));
    n.add("org/lwjgl/system/MemoryAccessJNI.malloc()J", |_, _| ret_j(malloc as *const () as usize as i64));
    n.add("org/lwjgl/system/MemoryAccessJNI.calloc()J", |_, _| ret_j(calloc as *const () as usize as i64));
    n.add("org/lwjgl/system/MemoryAccessJNI.realloc()J", |_, _| ret_j(realloc as *const () as usize as i64));
    n.add("org/lwjgl/system/MemoryAccessJNI.free()J", |_, _| ret_j(free as *const () as usize as i64));
    n.add("org/lwjgl/system/MemoryAccessJNI.aligned_alloc()J", |_, _| ret_j(aligned_alloc as *const () as usize as i64));
    n.add("org/lwjgl/system/MemoryAccessJNI.aligned_free()J", |_, _| ret_j(free as *const () as usize as i64));
    n.add("org/lwjgl/system/MemoryAccessJNI.ngetByte(J)B", |_, a| ret_i(get::<i8>(arg_j(a, 0)) as i32));
    n.add("org/lwjgl/system/MemoryAccessJNI.ngetShort(J)S", |_, a| ret_i(get::<i16>(arg_j(a, 0)) as i32));
    n.add("org/lwjgl/system/MemoryAccessJNI.ngetInt(J)I", |_, a| ret_i(get::<i32>(arg_j(a, 0))));
    n.add("org/lwjgl/system/MemoryAccessJNI.ngetLong(J)J", |_, a| ret_j(get::<i64>(arg_j(a, 0))));
    n.add("org/lwjgl/system/MemoryAccessJNI.ngetFloat(J)F", |_, a| ret_i(get::<u32>(arg_j(a, 0)) as i32));
    n.add("org/lwjgl/system/MemoryAccessJNI.ngetDouble(J)D", |_, a| ret_j(get::<i64>(arg_j(a, 0))));
    n.add("org/lwjgl/system/MemoryAccessJNI.ngetAddress(J)J", |_, a| ret_j(get::<u32>(arg_j(a, 0)) as i64));
    n.add("org/lwjgl/system/MemoryAccessJNI.nputByte(JB)V", |_, a| {
        put(arg_j(a, 0), a[2] as i8);
        ret_void()
    });
    n.add("org/lwjgl/system/MemoryAccessJNI.nputShort(JS)V", |_, a| {
        put(arg_j(a, 0), a[2] as i16);
        ret_void()
    });
    n.add("org/lwjgl/system/MemoryAccessJNI.nputInt(JI)V", |_, a| {
        put(arg_j(a, 0), a[2] as i32);
        ret_void()
    });
    n.add("org/lwjgl/system/MemoryAccessJNI.nputLong(JJ)V", |_, a| {
        put(arg_j(a, 0), a[2] as i64);
        ret_void()
    });
    n.add("org/lwjgl/system/MemoryAccessJNI.nputFloat(JF)V", |_, a| {
        put(arg_j(a, 0), a[2] as u32);
        ret_void()
    });
    n.add("org/lwjgl/system/MemoryAccessJNI.nputDouble(JD)V", |_, a| {
        put(arg_j(a, 0), a[2]);
        ret_void()
    });
    n.add("org/lwjgl/system/MemoryAccessJNI.nputAddress(JJ)V", |_, a| {
        put(arg_j(a, 0), a[2] as u32);
        ret_void()
    });

    n.add("org/lwjgl/system/MemoryUtil.ngetPageSize()I", |_, _| ret_i(4096));
    n.add("org/lwjgl/system/MemoryUtil.ngetCacheLineSize()I", |_, _| ret_i(64));
    n.add("org/lwjgl/system/MemoryUtil.memGlobalRefToObject(J)Ljava/lang/Object;", |_, a| ret_ref(global(arg_j(a, 0) as u64)));

    n.add("org/lwjgl/system/ThreadLocalUtil.getThreadJNIEnv()J", |_, _| ret_j(thread_env() as i64));
    n.add("org/lwjgl/system/ThreadLocalUtil.getFunctionMissingAbort()J", |_, _| ret_j(function_missing_abort as *const () as usize as i64));
    // A per-thread copy of the function table, installed in the thread's env.
    n.add("org/lwjgl/system/ThreadLocalUtil.setupEnvData(I)J", |_, a| {
        let count = (arg_i(a, 0) as usize).max(JNI_FUNCTIONS);
        // SAFETY: a new table of `count` pointers, initialised from the shared one.
        unsafe {
            let table = calloc(count, 4);
            std::ptr::copy_nonoverlapping(shared_table() as usize as *const u8, table as *mut u8, JNI_FUNCTIONS * 4);
            *(thread_env() as usize as *mut u32) = table as u32;
            ret_j(table as u32 as i64)
        }
    });

    n.add("org/lwjgl/system/SharedLibraryUtil.getLibraryPath(JJI)I", |_, a| {
        let path = libs::path(arg_j(a, 0) as u32);
        let (buffer, size) = (arg_j(a, 2) as usize as *mut u8, arg_i(a, 4) as usize);
        let n = path.len().min(size.saturating_sub(1));
        // SAFETY: LWJGL's buffer of `size` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(path.as_ptr(), buffer, n);
            *buffer.add(n) = 0;
        }
        ret_i(path.len() as i32 + 1)
    });
    n.add("org/lwjgl/system/linux/DynamicLinkLoader.ndlopen(JI)J", |_, a| ret_j(libs::open(c_str(arg_j(a, 0) as u32)) as i64));
    n.add("org/lwjgl/system/linux/DynamicLinkLoader.ndlsym(JJ)J", |_, a| {
        ret_j(libs::symbol(arg_j(a, 0) as u32, c_str(arg_j(a, 2) as u32)) as i64)
    });
    n.add("org/lwjgl/system/linux/DynamicLinkLoader.ndlclose(J)I", |_, _| ret_i(0));
    n.add("org/lwjgl/system/linux/DynamicLinkLoader.ndlerror()J", |_, _| ret_j(libs::error() as i64));

    // libc string functions, on addresses or on arrays.
    n.add("org/lwjgl/system/libc/LibCString.nmemset(JIJ)J", |_, a| {
        // SAFETY: LWJGL's contract.
        unsafe { std::ptr::write_bytes(arg_j(a, 0) as usize as *mut u8, arg_i(a, 2) as u8, arg_j(a, 3) as usize) };
        ret_j(arg_j(a, 0))
    });
    n.add("org/lwjgl/system/libc/LibCString.nmemcpy(JJJ)J", |_, a| {
        // SAFETY: as above.
        unsafe { std::ptr::copy_nonoverlapping(arg_j(a, 2) as usize as *const u8, arg_j(a, 0) as usize as *mut u8, arg_j(a, 4) as usize) };
        ret_j(arg_j(a, 0))
    });
    n.add("org/lwjgl/system/libc/LibCString.nmemmove(JJJ)J", |_, a| {
        // SAFETY: as above.
        unsafe { std::ptr::copy(arg_j(a, 2) as usize as *const u8, arg_j(a, 0) as usize as *mut u8, arg_j(a, 4) as usize) };
        ret_j(arg_j(a, 0))
    });
    n.add("org/lwjgl/system/libc/LibCString.nstrlen(J)J", |_, a| ret_j(c_str(arg_j(a, 0) as u32).len() as i64));
    for kind in ["B", "S", "I", "J", "F", "D"] {
        let key = |f: &str, d: &str| -> &'static str { Box::leak(format!("{LIBC}.{f}({d})J").into_boxed_str()) };
        n.add(key("nmemset", &format!("[{kind}IJ")), |t, a| {
            let p = super::invoke_support::array(t, a[0]);
            // SAFETY: Java arrays of at least the given byte count (LWJGL checks).
            unsafe { std::ptr::write_bytes(p as usize as *mut u8, arg_i(a, 1) as u8, arg_j(a, 2) as usize) };
            ret_j(p as i64)
        });
        n.add(key("nmemcpy", &format!("[{kind}[{kind}J")), |t, a| {
            let (d, s) = (super::invoke_support::array(t, a[0]), super::invoke_support::array(t, a[1]));
            // SAFETY: as above.
            unsafe { std::ptr::copy(s as usize as *const u8, d as usize as *mut u8, arg_j(a, 2) as usize) };
            ret_j(d as i64)
        });
        n.add(key("nmemmove", &format!("[{kind}[{kind}J")), |t, a| {
            let (d, s) = (super::invoke_support::array(t, a[0]), super::invoke_support::array(t, a[1]));
            // SAFETY: as above.
            unsafe { std::ptr::copy(s as usize as *const u8, d as usize as *mut u8, arg_j(a, 2) as usize) };
            ret_j(d as i64)
        });
    }
    n.add("org/lwjgl/system/libc/LibCStdlib.nmalloc(J)J", |_, a| ret_j(unsafe { malloc(arg_j(a, 0) as usize) } as u32 as i64));
    n.add("org/lwjgl/system/libc/LibCStdlib.ncalloc(JJ)J", |_, a| {
        ret_j(unsafe { calloc(arg_j(a, 0) as usize, arg_j(a, 2) as usize) } as u32 as i64)
    });
    n.add("org/lwjgl/system/libc/LibCStdlib.nrealloc(JJ)J", |_, a| {
        ret_j(unsafe { realloc(arg_j(a, 0) as usize as *mut std::ffi::c_void, arg_j(a, 2) as usize) } as u32 as i64)
    });
    n.add("org/lwjgl/system/libc/LibCStdlib.nfree(J)V", |_, a| {
        unsafe { free(arg_j(a, 0) as usize as *mut std::ffi::c_void) };
        ret_void()
    });
    n.add("org/lwjgl/system/libc/LibCStdlib.naligned_alloc(JJ)J", |_, a| {
        ret_j(unsafe { aligned_alloc(arg_j(a, 0) as usize, arg_j(a, 2) as usize) } as u32 as i64)
    });
    n.add("org/lwjgl/system/libc/LibCStdlib.naligned_free(J)V", |_, a| {
        unsafe { free(arg_j(a, 0) as usize as *mut std::ffi::c_void) };
        ret_void()
    });

    // The JNI functions LWJGL exposes to Java.
    n.add("org/lwjgl/system/jni/JNINativeInterface.GetVersion()I", |_, _| ret_i(0x0018_0000));
    n.add("org/lwjgl/system/jni/JNINativeInterface.NewGlobalRef(Ljava/lang/Object;)J", |_, a| ret_j(new_global(arg_ref(a, 0)) as i64));
    n.add("org/lwjgl/system/jni/JNINativeInterface.nDeleteGlobalRef(J)V", |_, a| {
        delete_global(arg_j(a, 0) as u64);
        ret_void()
    });
    n.add("org/lwjgl/system/jni/JNINativeInterface.NewWeakGlobalRef(Ljava/lang/Object;)J", |_, a| {
        ret_j(new_global(arg_ref(a, 0)) as i64)
    });
    n.add("org/lwjgl/system/jni/JNINativeInterface.nDeleteWeakGlobalRef(J)V", |_, a| {
        delete_global(arg_j(a, 0) as u64);
        ret_void()
    });
    n.add("org/lwjgl/system/jni/JNINativeInterface.nNewDirectByteBuffer(JJ)Ljava/nio/ByteBuffer;", |t, a| {
        let class = t.load_class("java/nio/DirectByteBuffer")?;
        t.ensure_initialized(class)?;
        let buffer = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
        crate::threads::push_handle(buffer);
        let init = class.find_method("<init>", "(JJ)V").expect("DirectByteBuffer(long, long)");
        t.invoke(init, &[buffer.slot(), a[0], 0, a[2], 0])?;
        ret_ref(buffer)
    });
    n.add("org/lwjgl/system/jni/JNINativeInterface.GetDirectBufferAddress(Ljava/nio/Buffer;)J", |t, a| {
        let buffer = t.null_check(arg_ref(a, 0))?;
        ret_j(t.field(buffer, "java/nio/Buffer", "address") as i64)
    });
    n.add("org/lwjgl/system/jni/JNINativeInterface.GetObjectRefType(Ljava/lang/Object;)I", |_, a| {
        ret_i(if arg_ref(a, 0).is_null() { 0 } else { 2 })
    });
    n.add("org/lwjgl/system/jni/JNINativeInterface.noop()V", no_op);
}
