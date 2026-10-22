//! java.lang.management backing natives (sun.management): enough for
//! RuntimeMXBean, ThreadMXBean and MemoryMXBean queries to answer sensibly.

use super::*;

pub(super) fn register(n: &mut Natives) {
    n.add("sun/management/VMManagementImpl.getVersion0()Ljava/lang/String;", |t, _| ret_ref(t.new_string("4.0")?));
    n.add("sun/management/VMManagementImpl.initOptionalSupportFields()V", no_op);
    n.add("sun/management/VMManagementImpl.isThreadContentionMonitoringEnabled()Z", return_false);
    n.add("sun/management/VMManagementImpl.isThreadCpuTimeEnabled()Z", return_false);
    n.add("sun/management/VMManagementImpl.isThreadAllocatedMemoryEnabled()Z", return_false);
    n.add("sun/management/VMManagementImpl.getAOTMode()Ljava/lang/String;", |t, _| ret_ref(t.new_string("off")?));
    n.add("sun/management/VMManagementImpl.isAOTRecording()Z", return_false);
    n.add("sun/management/VMManagementImpl.getAOTRecordingDuration()J", return_zero_long);
    n.add("sun/management/VMManagementImpl.endAOTRecording()Z", return_false);
    n.add("sun/management/VMManagementImpl.getTotalClassCount()J", |t, _| ret_j(t.vm.loaded_classes().len() as i64));
    n.add("sun/management/VMManagementImpl.getUnloadedClassCount()J", return_zero_long);
    n.add("sun/management/VMManagementImpl.getVerboseClass()Z", return_false);
    n.add("sun/management/VMManagementImpl.getVerboseGC()Z", return_false);
    n.add("sun/management/VMManagementImpl.getProcessId()I", |_, _| ret_i(crate::native::process_id() as i32));
    n.add("sun/management/VMManagementImpl.getVmArguments0()[Ljava/lang/String;", |t, _| ret_ref(t.new_object_array("java/lang/String", &[])?));
    n.add("sun/management/VMManagementImpl.getStartupTime()J", |t, _| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        ret_j(now - t.vm.start_time.elapsed().as_millis() as i64)
    });
    n.add("sun/management/VMManagementImpl.getUptime0()J", |t, _| ret_j(t.vm.start_time.elapsed().as_millis() as i64));
    n.add("sun/management/VMManagementImpl.getAvailableProcessors()I", |_, _| ret_i(1));
    n.add("sun/management/VMManagementImpl.getTotalCompileTime()J", return_zero_long);
    n.add("sun/management/VMManagementImpl.getTotalThreadCount()J", |t, _| ret_j(t.vm.threads.java_threads().len() as i64));
    n.add("sun/management/VMManagementImpl.getLiveThreadCount()I", |t, _| ret_i(t.vm.threads.java_threads().len() as i32));
    n.add("sun/management/VMManagementImpl.getPeakThreadCount()I", |t, _| ret_i(t.vm.threads.java_threads().len() as i32));
    n.add("sun/management/VMManagementImpl.getDaemonThreadCount()I", return_zero_int);
    for name in [
        "getSafepointCount",
        "getTotalSafepointTime",
        "getSafepointSyncTime",
        "getTotalApplicationNonStoppedTime",
        "getLoadedClassSize",
        "getUnloadedClassSize",
        "getClassLoadingTime",
        "getMethodDataSize",
        "getInitializedClassCount",
        "getClassInitializationTime",
        "getClassVerificationTime",
    ] {
        let key: &'static str = Box::leak(format!("sun/management/VMManagementImpl.{name}()J").into_boxed_str());
        n.add(key, return_zero_long);
    }

    n.add("sun/management/ThreadImpl.getThreads()[Ljava/lang/Thread;", |t, _| {
        let threads = t.vm.threads.java_threads();
        ret_ref(t.new_object_array("java/lang/Thread", &threads)?)
    });
    n.add("sun/management/ThreadImpl.getThreadTotalCpuTime0(J)J", |_, _| ret_j(-1));
    n.add("sun/management/ThreadImpl.getThreadUserCpuTime0(J)J", |_, _| ret_j(-1));
    n.add("sun/management/ThreadImpl.getThreadAllocatedMemory0(J)J", |_, _| ret_j(-1));
    n.add("sun/management/ThreadImpl.getTotalThreadAllocatedMemory()J", |_, _| ret_j(-1));
    n.add("sun/management/ThreadImpl.setThreadCpuTimeEnabled0(Z)V", no_op);
    n.add("sun/management/ThreadImpl.setThreadAllocatedMemoryEnabled0(Z)V", no_op);
    n.add("sun/management/ThreadImpl.setThreadContentionMonitoringEnabled0(Z)V", no_op);
    n.add("sun/management/ThreadImpl.findMonitorDeadlockedThreads0()[Ljava/lang/Thread;", return_null);
    n.add("sun/management/ThreadImpl.findDeadlockedThreads0()[Ljava/lang/Thread;", return_null);
    n.add("sun/management/ThreadImpl.resetPeakThreadCount0()V", no_op);
    n.add("sun/management/ThreadImpl.resetContentionTimes0(J)V", no_op);

    n.add("sun/management/MemoryImpl.getMemoryPools0()[Ljava/lang/management/MemoryPoolMXBean;", |t, _| {
        ret_ref(t.new_object_array("java/lang/management/MemoryPoolMXBean", &[])?)
    });
    n.add("sun/management/MemoryImpl.getMemoryManagers0()[Ljava/lang/management/MemoryManagerMXBean;", |t, _| {
        ret_ref(t.new_object_array("java/lang/management/MemoryManagerMXBean", &[])?)
    });
    n.add("sun/management/MemoryImpl.getMemoryUsage0(Z)Ljava/lang/management/MemoryUsage;", |t, a| {
        let (used, capacity) = if arg_z(a, 1) { (t.vm.heap.used() as u64, t.vm.heap.capacity() as u64) } else { (0, 0) };
        let class = t.load_class("java/lang/management/MemoryUsage")?;
        t.ensure_initialized(class)?;
        let usage = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
        let init = class.find_method("<init>", "(JJJJ)V").expect("MemoryUsage(long, long, long, long)");
        t.invoke(init, &[usage.slot(), 0, 0, used, 0, capacity, 0, capacity, 0])?;
        ret_ref(usage)
    });
    n.add("sun/management/MemoryImpl.setVerboseGC(Z)V", no_op);
}
