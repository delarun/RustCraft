//! java.lang.ClassLoader natives: defining classes (including hidden classes
//! for java.lang.invoke) and bootstrap lookups.

use super::*;
use crate::heap::ARRAY_BASE_OFFSET;

/// MethodHandles.Lookup.ClassOption / ClassLoader.defineClass0 flags.
const NESTMATE_CLASS: i32 = 0x1;
const HIDDEN_CLASS: i32 = 0x2;

pub(super) fn register(n: &mut Natives) {
    n.add("java/lang/ClassLoader.registerNatives()V", no_op);
    n.add(
        "java/lang/ClassLoader.defineClass0(Ljava/lang/ClassLoader;Ljava/lang/Class;Ljava/lang/String;[BIILjava/security/ProtectionDomain;ZILjava/lang/Object;)Ljava/lang/Class;",
        class_loader_define_class0,
    );
    n.add(
        "java/lang/ClassLoader.defineClass1(Ljava/lang/ClassLoader;Ljava/lang/String;[BIILjava/security/ProtectionDomain;Ljava/lang/String;)Ljava/lang/Class;",
        class_loader_define_class1,
    );
    n.add("java/lang/ClassLoader.findBootstrapClass(Ljava/lang/String;)Ljava/lang/Class;", |t, a| {
        let name = t.vm.string_value(t.null_check(arg_ref(a, 0))?);
        match t.vm.load_class(&name.replace('.', "/")) {
            Ok(class) => ret_ref(t.vm.mirror(class)),
            Err(_) => ret_ref(Ref::NULL),
        }
    });
    n.add("java/lang/ClassLoader.findLoadedClass0(Ljava/lang/String;)Ljava/lang/Class;", |t, a| {
        let loader = arg_ref(a, 0);
        let name = t.vm.string_value(t.null_check(arg_ref(a, 1))?);
        match t.vm.find_loaded_by(loader, &name.replace('.', "/")) {
            Some(class) => ret_ref(t.vm.mirror(class)),
            None => ret_ref(Ref::NULL),
        }
    });
}

fn class_bytes(t: &mut Thread<'_>, array: Ref, offset: i32, len: i32) -> JResult<Vec<u8>> {
    let array = t.null_check(array)?;
    let length = t.vm.heap.array_length(array);
    if offset < 0 || len < 0 || offset as i64 + len as i64 > length as i64 {
        return Err(t.throw_new("java/lang/ArrayIndexOutOfBoundsException", None));
    }
    Ok(t.vm.heap.bytes(array, ARRAY_BASE_OFFSET + offset as u32, len as usize).to_vec())
}

fn internal_name(t: &Thread<'_>, name: Ref) -> Option<String> {
    (!name.is_null()).then(|| t.vm.string_value(name).replace('.', "/"))
}

fn class_loader_define_class0(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let loader = arg_ref(a, 0);
    let lookup = t.vm.class_from_mirror(t.null_check(arg_ref(a, 1))?);
    let name = internal_name(t, arg_ref(a, 2));
    let bytes = class_bytes(t, arg_ref(a, 3), arg_i(a, 4), arg_i(a, 5))?;
    let initialize = arg_z(a, 7);
    let flags = arg_i(a, 8);
    let class_data = arg_ref(a, 9);
    let hidden = flags & HIDDEN_CLASS != 0;
    let vm = t.vm;
    let class = vm
        .define_class_bytes(name.as_deref(), &bytes, loader, lookup.module.clone(), hidden, &mut |n| load_for(t, loader, n))
        .map_err(|e| t.throw_vm_error(e))?;
    if flags & NESTMATE_CLASS != 0 {
        let host = t.nest_host(lookup)?;
        let _ = class.dynamic_nest_host.set(host);
    }
    let mirror = t.vm.mirror(class);
    set_protection_domain(t, mirror, arg_ref(a, 6));
    if !class_data.is_null() {
        t.set_field(mirror, "java/lang/Class", "classData", class_data.slot());
    }
    if initialize {
        t.ensure_initialized(class)?;
    }
    ret_ref(mirror)
}

fn class_loader_define_class1(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let loader = arg_ref(a, 0);
    let name = internal_name(t, arg_ref(a, 1));
    let bytes = class_bytes(t, arg_ref(a, 2), arg_i(a, 3), arg_i(a, 4))?;
    let vm = t.vm;
    let class = vm
        .define_class_bytes(name.as_deref(), &bytes, loader, None, false, &mut |n| load_for(t, loader, n))
        .map_err(|e| t.throw_vm_error(e))?;
    let mirror = t.vm.mirror(class);
    set_protection_domain(t, mirror, arg_ref(a, 5));
    ret_ref(mirror)
}

/// Class.protectionDomain, which the VM sets when it defines a class (read by
/// Class.getProtectionDomain: e.g. code sources, as Fabric's loader finds its
/// jars by them).
fn set_protection_domain(t: &mut Thread<'_>, mirror: Ref, domain: Ref) {
    if !domain.is_null() {
        t.set_field(mirror, "java/lang/Class", "protectionDomain", domain.slot());
    }
}

/// Loads a superclass or superinterface of a class being defined by `loader`.
fn load_for(t: &mut Thread<'_>, loader: Ref, name: &str) -> crate::vm::VmResult<ClassRef> {
    t.load_class_in(loader, name).map_err(|crate::interp::Throw(exception)| {
        let class = t.vm.class_of(exception).java_name();
        crate::vm::VmError::new("java/lang/NoClassDefFoundError", format!("{} (caused by {class})", name.replace('/', ".")))
    })
}
