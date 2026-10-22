//! jdk.internal.misc natives (CDS, VM) and java.lang.ref.Reference.

use super::*;

pub(super) fn register(n: &mut Natives) {
    // No class data sharing archive: report everything as disabled.
    n.add("jdk/internal/misc/CDS.getCDSConfigStatus()I", return_zero_int);
    n.add("jdk/internal/misc/CDS.logLambdaFormInvoker(Ljava/lang/String;)V", no_op);
    n.add("jdk/internal/misc/CDS.initializeFromArchive(Ljava/lang/Class;)V", no_op);
    n.add("jdk/internal/misc/CDS.defineArchivedModules(Ljava/lang/ClassLoader;Ljava/lang/ClassLoader;)V", no_op);
    n.add("jdk/internal/misc/CDS.getRandomSeedForDumping()J", return_zero_long);
    n.add("jdk/internal/misc/CDS.dumpClassList(Ljava/lang/String;)V", no_op);
    n.add("jdk/internal/misc/CDS.dumpDynamicArchive(Ljava/lang/String;)V", no_op);
    n.add("jdk/internal/misc/CDS.needsClassInitBarrier0(Ljava/lang/Class;)Z", return_false);

    n.add("jdk/internal/misc/VM.initialize()V", no_op);
    n.add("jdk/internal/misc/VM.latestUserDefinedLoader0()Ljava/lang/ClassLoader;", return_null);
    n.add("jdk/internal/misc/VM.getuid()J", |_, _| ret_j(1000));
    n.add("jdk/internal/misc/VM.geteuid()J", |_, _| ret_j(1000));
    n.add("jdk/internal/misc/VM.getgid()J", |_, _| ret_j(1000));
    n.add("jdk/internal/misc/VM.getegid()J", |_, _| ret_j(1000));
    n.add("jdk/internal/misc/VM.getNanoTimeAdjustment(J)J", vm_nano_time_adjustment);
    n.add("jdk/internal/misc/VM.getRuntimeArguments()[Ljava/lang/String;", |t, _| ret_ref(t.new_object_array("java/lang/String", &[])?));

    n.add("java/lang/ref/Finalizer.isFinalizationEnabled()Z", |_, _| ret_z(true));
    n.add("java/lang/ref/Reference.refersTo0(Ljava/lang/Object;)Z", reference_refers_to);
    n.add("java/lang/ref/PhantomReference.refersTo0(Ljava/lang/Object;)Z", reference_refers_to);
    n.add("java/lang/ref/Reference.clear0()V", reference_clear);
    n.add("java/lang/ref/PhantomReference.clear0()V", reference_clear);
    n.add("java/lang/ref/Reference.hasReferencePendingList()Z", return_false);
    n.add("java/lang/ref/Reference.getAndClearReferencePendingList()Ljava/lang/ref/Reference;", return_null);
    // The Reference Handler thread waits here; without a collector nothing is
    // ever pending, so it sleeps until interrupted.
    n.add("java/lang/ref/Reference.waitForReferencePendingList()V", |t, _| {
        t.wait_indefinitely();
        ret_void()
    });
    n.add("jdk/internal/vm/ContinuationSupport.isSupported0()Z", return_false);
    n.add("jdk/internal/misc/PreviewFeatures.isPreviewEnabled()Z", return_false);
    n.add("java/util/TimeZone.getSystemTimeZoneID(Ljava/lang/String;)Ljava/lang/String;", |t, _| {
        let zone = std::env::var("TZ").ok().filter(|z| !z.is_empty()).unwrap_or_else(|| "UTC".into());
        ret_ref(t.new_string(&zone)?)
    });
    n.add("java/util/TimeZone.getSystemGMTOffsetID()Ljava/lang/String;", |t, _| ret_ref(t.new_string("GMT")?));

    // Performance counters: plain native memory nobody reads.
    n.add("jdk/internal/perf/Perf.registerNatives()V", no_op);
    n.add("jdk/internal/perf/Perf.createLong(Ljava/lang/String;IIJ)Ljava/nio/ByteBuffer;", |t, a| {
        let buffer = new_direct_buffer(t, 8)?;
        let address = t.field(buffer, "java/nio/Buffer", "address");
        // SAFETY: freshly allocated 8-byte native block.
        unsafe { (address as usize as *mut i64).write_unaligned(arg_j(a, 4)) };
        ret_ref(buffer)
    });
    n.add("jdk/internal/perf/Perf.createByteArray(Ljava/lang/String;II[BI)Ljava/nio/ByteBuffer;", |t, a| {
        ret_ref(new_direct_buffer(t, arg_i(a, 5).max(1) as usize)?)
    });
    n.add("jdk/internal/perf/Perf.highResCounter()J", |t, _| ret_j(t.vm.start_time.elapsed().as_nanos() as i64));
    n.add("jdk/internal/perf/Perf.highResFrequency()J", |_, _| ret_j(1_000_000_000));
    n.add("jdk/internal/perf/Perf.detach(Ljava/nio/ByteBuffer;)V", no_op);

    // The runtime image is already in memory; hand it out without copying.
    n.add("jdk/internal/jimage/NativeImageBuffer.getNativeMap(Ljava/lang/String;)Ljava/nio/ByteBuffer;", |t, a| {
        let path = t.vm.string_value(t.null_check(arg_ref(a, 0))?);
        let expected = format!("{}/lib/modules", t.vm.config.java_home);
        match t.vm.image().filter(|_| crate::fs::normalize(&path) == expected) {
            Some(image) => {
                let data = image.data();
                ret_ref(wrap_direct_buffer(t, data.as_ptr() as usize, data.len())?)
            }
            None => ret_ref(Ref::NULL),
        }
    });
}

/// Nanoseconds since `offset_seconds` past the epoch, or -1 if out of range.
fn vm_nano_time_adjustment(_: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let offset_seconds = arg_j(a, 0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let seconds = now.as_secs() as i64 - offset_seconds;
    // HotSpot's limit: |seconds| must fit in ~2^32.
    if !(-(1i64 << 32)..=(1i64 << 32)).contains(&seconds) {
        return ret_j(-1);
    }
    ret_j(seconds * 1_000_000_000 + now.subsec_nanos() as i64)
}

fn reference_clear(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    t.set_field(arg_ref(a, 0), "java/lang/ref/Reference", "referent", 0);
    ret_void()
}

fn reference_refers_to(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let referent = t.field_ref(arg_ref(a, 0), "java/lang/ref/Reference", "referent");
    ret_z(referent == arg_ref(a, 1))
}

/// A direct ByteBuffer over newly allocated, zeroed native memory (as JNI's
/// NewDirectByteBuffer). The memory is never freed.
pub(crate) fn new_direct_buffer(t: &mut Thread<'_>, capacity: usize) -> JResult<Ref> {
    let memory: &'static mut [u8] = Box::leak(vec![0u8; capacity].into_boxed_slice());
    wrap_direct_buffer(t, memory.as_mut_ptr() as usize, capacity)
}

/// A direct ByteBuffer over existing native memory that outlives it.
pub(crate) fn wrap_direct_buffer(t: &mut Thread<'_>, address: usize, capacity: usize) -> JResult<Ref> {
    let class = t.load_class("java/nio/DirectByteBuffer")?;
    t.ensure_initialized(class)?;
    let buffer = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
    let init = class.find_method("<init>", "(JJ)V").expect("DirectByteBuffer(long, long)");
    t.invoke(init, &[buffer.slot(), address as u64, 0, capacity as u64, 0])?;
    Ok(buffer)
}
