//! java.lang natives: Object, System, Class, Throwable, StackTraceElement,
//! String, Float/Double, Runtime.

use super::*;
use crate::class::{ClassKind, FieldKind};
use crate::heap::ARRAY_BASE_OFFSET;
use std::io::Write;

pub(super) fn register(n: &mut Natives) {
    n.add("java/lang/Object.getClass()Ljava/lang/Class;", object_get_class);
    n.add("java/lang/Object.hashCode()I", object_hash_code);
    n.add("java/lang/Object.clone()Ljava/lang/Object;", object_clone);
    n.add("java/lang/Object.notify()V", |t, a| {
        t.monitor_notify(arg_ref(a, 0), false)?;
        ret_void()
    });
    n.add("java/lang/Object.notifyAll()V", |t, a| {
        t.monitor_notify(arg_ref(a, 0), true)?;
        ret_void()
    });
    n.add("java/lang/Object.wait0(J)V", |t, a| {
        t.monitor_wait(arg_ref(a, 0), arg_j(a, 1))?;
        ret_void()
    });

    n.add("java/lang/System.registerNatives()V", no_op);
    n.add("java/lang/System.arraycopy(Ljava/lang/Object;ILjava/lang/Object;II)V", system_arraycopy);
    n.add("java/lang/System.identityHashCode(Ljava/lang/Object;)I", system_identity_hash_code);
    n.add("java/lang/System.currentTimeMillis()J", system_current_time_millis);
    n.add("java/lang/System.nanoTime()J", system_nano_time);
    n.add("java/lang/System.setIn0(Ljava/io/InputStream;)V", |t, a| set_system_stream(t, "in", arg_ref(a, 0)));
    n.add("java/lang/System.setOut0(Ljava/io/PrintStream;)V", |t, a| set_system_stream(t, "out", arg_ref(a, 0)));
    n.add("java/lang/System.setErr0(Ljava/io/PrintStream;)V", |t, a| set_system_stream(t, "err", arg_ref(a, 0)));
    n.add("java/lang/System.mapLibraryName(Ljava/lang/String;)Ljava/lang/String;", system_map_library_name);

    n.add("java/lang/Class.registerNatives()V", no_op);
    n.add("java/lang/Class.desiredAssertionStatus0(Ljava/lang/Class;)Z", return_false);
    n.add("java/lang/Class.getPrimitiveClass(Ljava/lang/String;)Ljava/lang/Class;", class_get_primitive_class);
    n.add("java/lang/Class.isInstance(Ljava/lang/Object;)Z", class_is_instance);
    n.add("java/lang/Class.isAssignableFrom(Ljava/lang/Class;)Z", class_is_assignable_from);
    n.add("java/lang/Class.initClassName()Ljava/lang/String;", class_init_class_name);
    n.add("java/lang/Class.getSuperclass()Ljava/lang/Class;", class_get_superclass);
    n.add("java/lang/Class.getInterfaces0()[Ljava/lang/Class;", class_get_interfaces);
    n.add("java/lang/Class.isHidden()Z", |t, a| ret_z(this_class(t, a).hidden));
    n.add("java/lang/Class.isRecord0()Z", class_is_record);
    n.add("java/lang/Class.getClassAccessFlagsRaw0()I", class_access_flags_raw);
    n.add("java/lang/Class.getClassFileVersion0()I", class_file_version);
    n.add("java/lang/Class.getDeclaringClass0()Ljava/lang/Class;", class_get_declaring_class);
    n.add("java/lang/Class.getSimpleBinaryName0()Ljava/lang/String;", class_get_simple_binary_name);
    n.add("java/lang/Class.getEnclosingMethod0()[Ljava/lang/Object;", class_get_enclosing_method);
    n.add("java/lang/Class.getGenericSignature0()Ljava/lang/String;", class_get_generic_signature);
    n.add("java/lang/Class.getRawAnnotations()[B", |t, a| class_raw_attribute(t, a, "RuntimeVisibleAnnotations"));
    n.add("java/lang/Class.getRawTypeAnnotations()[B", |t, a| class_raw_attribute(t, a, "RuntimeVisibleTypeAnnotations"));
    n.add("java/lang/Class.getNestHost0()Ljava/lang/Class;", class_get_nest_host);
    n.add("java/lang/Class.forName0(Ljava/lang/String;ZLjava/lang/ClassLoader;Ljava/lang/Class;)Ljava/lang/Class;", class_for_name0);

    n.add("java/lang/Throwable.fillInStackTrace(I)Ljava/lang/Throwable;", throwable_fill_in_stack_trace);
    // Helpful NPE messages (JEP 358) are optional; null means none.
    n.add("java/lang/NullPointerException.getExtendedNPEMessage()Ljava/lang/String;", |_, _| ret_ref(Ref::NULL));
    n.add(
        "java/lang/StackTraceElement.initStackTraceElements([Ljava/lang/StackTraceElement;Ljava/lang/Object;I)V",
        stack_trace_element_init_all,
    );

    n.add("java/lang/String.intern()Ljava/lang/String;", |t, a| ret_ref(t.vm.intern(arg_ref(a, 0))));

    n.add("java/lang/Float.floatToRawIntBits(F)I", |_, a| ret_i(a[0] as i32));
    n.add("java/lang/Float.intBitsToFloat(I)F", |_, a| ret_i(a[0] as i32));
    n.add("java/lang/Double.doubleToRawLongBits(D)J", |_, a| ret_j(a[0] as i64));
    n.add("java/lang/Double.longBitsToDouble(J)D", |_, a| ret_j(a[0] as i64));

    // Threads run in parallel: report the host's cores (RJAVA_CPUS overrides).
    n.add("java/lang/Runtime.availableProcessors()I", |_, _| ret_i(crate::processors() as i32));
    n.add("java/lang/ProcessEnvironment.environ()[[B", process_environ);
    n.add("java/lang/ProcessImpl.init()V", no_op);
    n.add("java/lang/Runtime.freeMemory()J", |t, _| ret_j((t.vm.heap.capacity() - t.vm.heap.used()) as i64));
    n.add("java/lang/Runtime.totalMemory()J", |t, _| ret_j(t.vm.heap.capacity() as i64));
    n.add("java/lang/Runtime.maxMemory()J", |t, _| ret_j(t.vm.heap.capacity() as i64));
    n.add("java/lang/Runtime.gc()V", no_op);

    // Test hook: lets VM tests print before System.out is initialised. The Java
    // implementation (used when the same test runs on a reference JVM) is replaced.
    n.add("vmtest/Out.println(Ljava/lang/String;)V", out_println);
}

fn object_get_class(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = t.vm.class_of(arg_ref(a, 0));
    ret_ref(t.vm.mirror(class))
}

fn object_hash_code(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    ret_i(t.vm.identity_hash(arg_ref(a, 0)))
}

fn object_clone(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let obj = arg_ref(a, 0);
    let class = t.vm.class_of(obj);
    let cloneable = t.load_class("java/lang/Cloneable")?;
    if !class.is_subclass_of(cloneable) {
        return Err(t.throw_new("java/lang/CloneNotSupportedException", Some(&class.java_name())));
    }
    let copy = t.vm.clone_object(obj).map_err(|e| t.throw_vm_error(e))?;
    ret_ref(copy)
}

fn system_identity_hash_code(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let obj = arg_ref(a, 0);
    ret_i(if obj.is_null() { 0 } else { t.vm.identity_hash(obj) })
}

fn system_current_time_millis(_: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    ret_j(now.as_millis() as i64)
}

fn system_nano_time(t: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    ret_j(t.vm.start_time.elapsed().as_nanos() as i64)
}

fn set_system_stream(t: &mut Thread<'_>, name: &str, stream: Ref) -> JResult<Option<u64>> {
    let system = t.load_class("java/lang/System")?;
    t.set_static(system, name, stream.slot());
    ret_void()
}

fn system_map_library_name(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let name = t.null_check(arg_ref(a, 0))?;
    let mapped = format!("lib{}.so", t.vm.string_value(name));
    ret_ref(t.new_string(&mapped)?)
}

/// System.arraycopy when it cannot fail and needs no element checks: arrays
/// of one element kind (references: the source's class a subclass of the
/// destination's), positions and length in bounds. False: nothing done.
pub(crate) fn arraycopy_unchecked(vm: &crate::vm::Vm, src: Ref, src_pos: i32, dst: Ref, dst_pos: i32, length: i32) -> bool {
    if src.is_null() || dst.is_null() || (src_pos | dst_pos | length) < 0 {
        return false;
    }
    let (src_class, dst_class) = (vm.class_of(src), vm.class_of(dst));
    let (Some(kind), Some(dst_kind)) = (src_class.element_kind(), dst_class.element_kind()) else { return false };
    if kind != dst_kind || (kind == FieldKind::Ref && !std::ptr::eq(src_class, dst_class) && !src_class.is_subclass_of(dst_class)) {
        return false;
    }
    let heap = &vm.heap;
    if src_pos as i64 + length as i64 > heap.array_length(src) as i64 || dst_pos as i64 + length as i64 > heap.array_length(dst) as i64 {
        return false;
    }
    let size = kind.size();
    heap.copy(src, ARRAY_BASE_OFFSET + src_pos as u32 * size, dst, ARRAY_BASE_OFFSET + dst_pos as u32 * size, (length as u32 * size) as usize);
    true
}

fn system_arraycopy(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let (src, src_pos, dst, dst_pos, length) = (arg_ref(a, 0), arg_i(a, 1), arg_ref(a, 2), arg_i(a, 3), arg_i(a, 4));
    if src.is_null() || dst.is_null() {
        return Err(t.throw_new("java/lang/NullPointerException", None));
    }
    let (src_class, dst_class) = (t.vm.class_of(src), t.vm.class_of(dst));
    let (Some(src_kind), Some(dst_kind)) = (src_class.element_kind(), dst_class.element_kind()) else {
        let which = if src_class.is_array() { "destination" } else { "source" };
        let message = format!("arraycopy: {which} type {} is not an array", if src_class.is_array() { dst_class.java_name() } else { src_class.java_name() });
        return Err(t.throw_new("java/lang/ArrayStoreException", Some(&message)));
    };
    if src_kind != dst_kind {
        let message = format!("arraycopy: type mismatch: can not copy {}[] into {}[]", type_label(src_kind), type_label(dst_kind));
        return Err(t.throw_new("java/lang/ArrayStoreException", Some(&message)));
    }
    let (src_len, dst_len) = (t.vm.heap.array_length(src), t.vm.heap.array_length(dst));
    let check = |pos: i32, len: i32, what: &str| -> Option<String> {
        if pos < 0 {
            Some(format!("arraycopy: {what} index {pos} out of bounds for {}[{len}]", type_label(src_kind)))
        } else if length < 0 {
            Some(format!("arraycopy: length {length} is negative"))
        } else if pos as i64 + length as i64 > len as i64 {
            Some(format!("arraycopy: last {what} index {} out of bounds for {}[{len}]", pos as i64 + length as i64, type_label(src_kind)))
        } else {
            None
        }
    };
    if let Some(message) = check(src_pos, src_len, "source").or_else(|| check(dst_pos, dst_len, "destination")) {
        return Err(t.throw_new("java/lang/ArrayIndexOutOfBoundsException", Some(&message)));
    }
    let size = src_kind.size();
    let heap = &t.vm.heap;
    if src_kind != FieldKind::Ref || src_class.is_subclass_of(dst_class) {
        heap.copy(src, ARRAY_BASE_OFFSET + src_pos as u32 * size, dst, ARRAY_BASE_OFFSET + dst_pos as u32 * size, (length as u32 * size) as usize);
        return ret_void();
    }
    // Reference arrays of different types: check each element.
    let component = dst_class.component().unwrap();
    for i in 0..length {
        let element = Ref(heap.read(src, ARRAY_BASE_OFFSET + (src_pos + i) as u32 * 4));
        if !element.is_null() && !t.vm.class_of(element).is_subclass_of(component) {
            let message = format!(
                "arraycopy: element type mismatch: can not cast one of the elements of {}[] to the type of the destination array, {}",
                src_class.component().unwrap().java_name(),
                component.java_name()
            );
            return Err(t.throw_new("java/lang/ArrayStoreException", Some(&message)));
        }
        heap.write(dst, ARRAY_BASE_OFFSET + (dst_pos + i) as u32 * 4, element.0);
    }
    ret_void()
}

fn type_label(kind: FieldKind) -> &'static str {
    if kind == FieldKind::Ref { "object array" } else { kind.primitive_name() }
}

fn this_class(t: &Thread<'_>, a: &[u64]) -> ClassRef {
    t.vm.class_from_mirror(arg_ref(a, 0))
}

fn class_get_primitive_class(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let name = t.vm.string_value(arg_ref(a, 0));
    match t.vm.primitive_class_by_name(&name) {
        Some(class) => ret_ref(t.vm.mirror(class)),
        None => Err(t.throw_new("java/lang/IllegalArgumentException", Some(&name))),
    }
}

fn class_is_instance(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let obj = arg_ref(a, 1);
    ret_z(!obj.is_null() && t.vm.class_of(obj).is_subclass_of(class))
}

fn class_is_assignable_from(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let other = t.null_check(arg_ref(a, 1))?;
    let other = t.vm.class_from_mirror(other);
    if class.is_primitive() || other.is_primitive() {
        return ret_z(std::ptr::eq(class, other));
    }
    ret_z(other.is_subclass_of(class))
}

fn class_init_class_name(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let name = t.new_string(&class.java_name())?;
    let name = t.vm.intern(name);
    t.set_field(arg_ref(a, 0), "java/lang/Class", "name", name.slot());
    ret_ref(name)
}

fn class_get_superclass(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let superclass = if class.is_interface() || class.is_primitive() { None } else { class.super_class };
    ret_ref(superclass.map_or(Ref::NULL, |s| t.vm.mirror(s)))
}

fn class_get_interfaces(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let mirrors: Vec<Ref> = class.interfaces.iter().map(|&i| t.vm.mirror(i)).collect();
    ret_ref(t.new_object_array("java/lang/Class", &mirrors)?)
}

fn class_is_record(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    ret_z(class.file.as_ref().is_some_and(|f| f.record_components.is_some()))
}

fn class_access_flags_raw(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    ret_i(this_class(t, a).access_flags as i32)
}

fn class_file_version(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let class = match class.kind {
        ClassKind::Array { .. } => t.load_class("java/lang/Object")?,
        _ => class,
    };
    ret_i(class.file.as_ref().map_or(0, |f| f.major_version as i32 | (f.minor_version as i32) << 16))
}

fn inner_class_entry(class: ClassRef) -> Option<&'static classfile::InnerClass> {
    class.file.as_ref()?.inner_classes.iter().find(|ic| ic.inner_class == class.name)
}

fn class_get_declaring_class(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let Some(outer) = inner_class_entry(class).and_then(|ic| ic.outer_class.as_deref()) else {
        return ret_ref(Ref::NULL);
    };
    let outer = t.load_class_in(class.loader, outer)?;
    ret_ref(t.vm.mirror(outer))
}

fn class_get_simple_binary_name(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    match inner_class_entry(class).and_then(|ic| ic.inner_name.as_deref()) {
        Some(name) => ret_ref(t.new_string(name)?),
        None => ret_ref(Ref::NULL),
    }
}

fn class_get_enclosing_method(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let Some(enclosing) = class.file.as_ref().and_then(|f| f.enclosing_method.as_ref()) else {
        return ret_ref(Ref::NULL);
    };
    let outer = t.load_class_in(class.loader, &enclosing.class)?;
    let mut items = vec![t.vm.mirror(outer), Ref::NULL, Ref::NULL];
    if let Some((name, descriptor)) = &enclosing.method {
        items[1] = t.new_string(name)?;
        items[2] = t.new_string(descriptor)?;
    }
    ret_ref(t.new_object_array("java/lang/Object", &items)?)
}

fn class_get_generic_signature(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    match class.file.as_ref().and_then(|f| f.signature.as_deref()) {
        Some(signature) => ret_ref(t.new_string(signature)?),
        None => ret_ref(Ref::NULL),
    }
}

fn class_raw_attribute(t: &mut Thread<'_>, a: &[u64], name: &str) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let Some(data) = class.file.as_ref().and_then(|f| f.attribute(name)) else {
        return ret_ref(Ref::NULL);
    };
    ret_ref(new_byte_array(t, data)?)
}

pub(crate) fn new_byte_array(t: &mut Thread<'_>, data: &[u8]) -> JResult<Ref> {
    let array = t.vm.new_primitive_array(FieldKind::Byte, data.len() as i32).map_err(|e| t.throw_vm_error(e))?;
    t.vm.heap.bytes_mut(array, ARRAY_BASE_OFFSET, data.len()).copy_from_slice(data);
    Ok(array)
}

fn class_get_nest_host(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let host = t.nest_host(class)?;
    ret_ref(t.vm.mirror(host))
}

fn class_for_name0(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let name = t.null_check(arg_ref(a, 0))?;
    let initialize = arg_z(a, 1);
    let loader = arg_ref(a, 2);
    let java_name = t.vm.string_value(name);
    if java_name.contains('/') {
        return Err(t.throw_new("java/lang/ClassNotFoundException", Some(&java_name)));
    }
    let class = if loader.is_null() {
        match t.vm.load_class(&java_name.replace('.', "/")) {
            Ok(class) => class,
            Err(_) => return Err(t.throw_new("java/lang/ClassNotFoundException", Some(&java_name))),
        }
    } else if java_name.starts_with('[') {
        // Arrays are made by the VM; only the element type is loaded (by the
        // loader), as in HotSpot (e.g. dynamic proxies look up "[Ljava.lang.Object;").
        match t.load_class_in(loader, &java_name.replace('.', "/")) {
            Ok(class) => class,
            Err(_) => {
                // (HotSpot names the element class.)
                let element = java_name.trim_start_matches('[');
                let element = element.strip_prefix('L').and_then(|e| e.strip_suffix(';')).unwrap_or(&java_name);
                return Err(t.throw_new("java/lang/ClassNotFoundException", Some(element)));
            }
        }
    } else {
        // Delegates to loader.loadClass, which throws ClassNotFoundException itself.
        let java_string = t.new_string(&java_name)?;
        let mirror = t.invoke_virtual(loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[java_string.slot()])?;
        t.vm.class_from_mirror(t.null_check(Ref::from_slot(mirror.unwrap()))?)
    };
    if initialize {
        t.ensure_initialized(class)?;
    }
    ret_ref(t.vm.mirror(class))
}

// --- stack traces ---------------------------------------------------------------

fn throwable_fill_in_stack_trace(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let throwable = arg_ref(a, 0);
    let exception_class = t.vm.class_of(throwable);
    let frames = t.stack_trace();
    let throwable_class = t.load_class("java/lang/Throwable")?;
    let mut skip = 0;
    // Skip fillInStackTrace() itself, then the exception's constructors.
    while skip < frames.len()
        && frames[skip].0.name == "fillInStackTrace"
        && frames[skip].0.class().is_subclass_of(throwable_class)
    {
        skip += 1;
    }
    while skip < frames.len() && frames[skip].0.name == "<init>" && exception_class.is_subclass_of(frames[skip].0.class()) {
        skip += 1;
    }
    let frames: Vec<_> = frames[skip..].iter().copied().filter(|(m, _)| !super::stackwalk::is_hidden_frame(*m)).collect();
    let frames = &frames[..];
    let backtrace = t.vm.new_primitive_array(FieldKind::Int, frames.len() as i32 * 2).map_err(|e| t.throw_vm_error(e))?;
    for (i, (method, pc)) in frames.iter().enumerate() {
        t.vm.heap.write(backtrace, ARRAY_BASE_OFFSET + i as u32 * 8, method.global_index);
        t.vm.heap.write(backtrace, ARRAY_BASE_OFFSET + i as u32 * 8 + 4, *pc);
    }
    t.set_field(throwable, "java/lang/Throwable", "backtrace", backtrace.slot());
    t.set_field(throwable, "java/lang/Throwable", "depth", frames.len() as u64);
    ret_ref(throwable)
}

fn stack_trace_element_init_all(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let (elements, backtrace, depth) = (arg_ref(a, 0), arg_ref(a, 1), arg_i(a, 2));
    let elements = t.null_check(elements)?;
    let backtrace = t.null_check(backtrace)?;
    for i in 0..depth as u32 {
        let element = Ref(t.vm.heap.read(elements, ARRAY_BASE_OFFSET + i * 4));
        crate::threads::push_handle(element);
        let method = t.vm.method_by_index(t.vm.heap.read(backtrace, ARRAY_BASE_OFFSET + i * 8));
        let pc: u32 = t.vm.heap.read(backtrace, ARRAY_BASE_OFFSET + i * 8 + 4);
        fill_stack_trace_element(t, element, method, pc)?;
    }
    ret_void()
}

pub(crate) fn fill_stack_trace_element(t: &mut Thread<'_>, element: Ref, method: crate::class::MethodRef, pc: u32) -> JResult<()> {
    let class = method.class();
    let ste = "java/lang/StackTraceElement";
    t.set_field(element, ste, "declaringClassObject", t.vm.mirror(class).slot());
    let declaring = t.new_string(&class.java_name())?;
    t.set_field(element, ste, "declaringClass", declaring.slot());
    let name = t.new_string(&method.name)?;
    t.set_field(element, ste, "methodName", name.slot());
    if let Some(module) = &class.module {
        let module = t.new_string(module)?;
        t.set_field(element, ste, "moduleName", module.slot());
    }
    if let Some(file) = class.file.as_ref().and_then(|f| f.source_file.as_deref()) {
        let file = t.new_string(file)?;
        t.set_field(element, ste, "fileName", file.slot());
    }
    let line = if method.is_native() { -2 } else { method.line_number(pc).map_or(-1, |l| l as i32) };
    t.set_field(element, ste, "lineNumber", line as u32 as u64);
    Ok(())
}

fn out_println(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let s = arg_ref(a, 0);
    let text = if s.is_null() { "null".to_owned() } else { t.vm.string_value(s) };
    let mut out = t.vm.stdout.lock().unwrap();
    let _ = writeln!(out, "{text}");
    ret_void()
}

/// The process environment as alternating name/value byte arrays. Only a
/// curated set is exposed (the browser has no environment).
fn process_environ(t: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    let config = &t.vm.config;
    let vars = [("HOME", config.user_home.clone()), ("USER", config.user_name.clone()), ("LANG", "en_US.UTF-8".to_owned())];
    let mut items = Vec::with_capacity(vars.len() * 2);
    for (name, value) in vars {
        items.push(new_byte_array(t, name.as_bytes())?);
        items.push(new_byte_array(t, value.as_bytes())?);
    }
    ret_ref(t.new_object_array("[B", &items)?)
}
