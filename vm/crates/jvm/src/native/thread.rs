//! java.lang.Thread natives.
//! The machinery (OS threads, GIL, monitors, parking) lives in `threads.rs`.

use super::*;
use std::sync::atomic::AtomicU64;

/// Thread status of a running thread (JVMTI_THREAD_STATE_ALIVE | RUNNABLE).
pub(crate) const THREAD_STATUS_RUNNABLE: u64 = crate::threads::status::RUNNABLE as u64;

pub(super) fn register(n: &mut Natives) {
    n.add("java/lang/Thread.registerNatives()V", no_op);
    n.add("java/lang/Thread.currentThread()Ljava/lang/Thread;", |t, _| ret_ref(t.java_thread));
    n.add("java/lang/Thread.currentCarrierThread()Ljava/lang/Thread;", |t, _| ret_ref(t.java_thread));
    n.add("java/lang/Thread.setCurrentThread(Ljava/lang/Thread;)V", |t, a| {
        t.java_thread = arg_ref(a, 1);
        ret_void()
    });
    n.add("java/lang/Thread.scopedValueCache()[Ljava/lang/Object;", |t, _| ret_ref(t.scoped_value_cache));
    n.add("java/lang/Thread.setScopedValueCache([Ljava/lang/Object;)V", |t, a| {
        t.scoped_value_cache = arg_ref(a, 0);
        ret_void()
    });
    n.add("java/lang/Thread.findScopedValueBindings()Ljava/lang/Object;", return_null);
    n.add("java/lang/Thread.ensureMaterializedForStackWalk(Ljava/lang/Object;)V", no_op);
    n.add("java/lang/Thread.yield0()V", |t, _| {
        t.yield_now();
        ret_void()
    });
    n.add("java/lang/Thread.sleepNanos0(J)V", |t, a| {
        t.sleep_nanos(arg_j(a, 0))?;
        ret_void()
    });
    n.add("java/lang/Thread.start0()V", |t, a| {
        t.start_thread(arg_ref(a, 0))?;
        ret_void()
    });
    n.add("java/lang/Thread.holdsLock(Ljava/lang/Object;)Z", |t, a| {
        let obj = t.null_check(arg_ref(a, 0))?;
        ret_z(t.holds_lock(obj))
    });
    n.add("java/lang/Thread.getThreads()[Ljava/lang/Thread;", |t, _| {
        let threads = t.vm.threads.java_threads();
        ret_ref(t.new_object_array("java/lang/Thread", &threads)?)
    });
    n.add("java/lang/Thread.setPriority0(I)V", no_op);
    n.add("java/lang/Thread.interrupt0()V", |t, a| {
        // The Java side has set `interrupted`; wake the target if it is blocked.
        let id = t.field(arg_ref(a, 0), "java/lang/Thread", "eetop");
        if let Some(target) = t.vm.threads.get(id) {
            target.wake();
            target.unpark();
        }
        ret_void()
    });
    n.add("java/lang/Thread.clearInterruptEvent()V", no_op);
    n.add("java/lang/Thread.setNativeName(Ljava/lang/String;)V", no_op);
    n.add("java/lang/Thread.getNextThreadIdOffset()J", |_, _| {
        // ThreadIdentifiers increments this with Unsafe.getAndAddLong(null, address, 1).
        static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);
        ret_j(NEXT_THREAD_ID.as_ptr() as usize as i64)
    });
}

pub(super) fn register_shutdown(n: &mut Natives) {
    n.add("java/lang/Shutdown.beforeHalt()V", no_op);
    n.add("java/lang/Shutdown.halt0(I)V", |t, a| {
        let status = arg_i(a, 0);
        t.vm.halt(status);
        ret_void()
    });
}
