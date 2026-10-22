//! java.lang.Module and boot loader natives. The VM tracks which Module object
//! owns each package so that `Class.getModule()` works; access between modules
//! is not enforced by the VM.

use super::*;

pub(super) fn register(n: &mut Natives) {
    n.add("java/lang/Module.defineModule0(Ljava/lang/Module;ZLjava/lang/String;Ljava/lang/String;[Ljava/lang/Object;)V", module_define);
    n.add("java/lang/Module.addReads0(Ljava/lang/Module;Ljava/lang/Module;)V", no_op);
    n.add("java/lang/Module.addExports0(Ljava/lang/Module;Ljava/lang/String;Ljava/lang/Module;)V", no_op);
    n.add("java/lang/Module.addExportsToAll0(Ljava/lang/Module;Ljava/lang/String;)V", no_op);
    n.add("java/lang/Module.addExportsToAllUnnamed0(Ljava/lang/Module;Ljava/lang/String;)V", no_op);

    n.add("jdk/internal/loader/BootLoader.setBootLoaderUnnamedModule0(Ljava/lang/Module;)V", |t, a| {
        let module = t.null_check(arg_ref(a, 0))?;
        t.vm.set_boot_unnamed_module(module);
        ret_void()
    });
    n.add("jdk/internal/loader/BootLoader.getSystemPackageNames()[Ljava/lang/String;", |t, _| {
        let mut packages: Vec<String> = t
            .vm
            .loaded_classes()
            .iter()
            .filter(|c| c.loader.is_null() && c.kind.is_instance() && !c.hidden)
            .map(|c| c.package().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        packages.sort();
        packages.dedup();
        let mut strings = Vec::with_capacity(packages.len());
        for package in &packages {
            strings.push(t.new_string(package)?);
        }
        ret_ref(t.new_object_array("java/lang/String", &strings)?)
    });
    n.add("jdk/internal/loader/BootLoader.getSystemPackageLocation(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let package = t.vm.string_value(t.null_check(arg_ref(a, 0))?).replace('.', "/");
        let module = t.vm.image().and_then(|image| image.package_modules(&package).ok()).and_then(|m| m.into_iter().next());
        match module {
            Some(module) => ret_ref(t.new_string(&format!("jrt:/{module}"))?),
            None => ret_ref(Ref::NULL),
        }
    });

    // The JDK's own native code is linked into the VM ("built in"); other
    // libraries are loaded for real (JNI, natively).
    n.add("jdk/internal/loader/NativeLibraries.findBuiltinLib(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let name = t.vm.string_value(t.null_check(arg_ref(a, 0))?);
        let base = name.strip_prefix("lib").unwrap_or(&name);
        let base = base.strip_suffix(".so").unwrap_or(base);
        if JDK_LIBRARIES.contains(&base) || cfg!(not(unix)) {
            ret_ref(t.new_string(base)?)
        } else {
            ret_ref(Ref::NULL)
        }
    });
    n.add("jdk/internal/loader/NativeLibraries.load(Ljdk/internal/loader/NativeLibraries$NativeLibraryImpl;Ljava/lang/String;ZZ)Z", |t, a| {
        const IMPL: &str = "jdk/internal/loader/NativeLibraries$NativeLibraryImpl";
        let library = t.null_check(arg_ref(a, 0))?;
        #[cfg_attr(not(unix), allow(unused_variables))]
        let name = t.vm.string_value(t.null_check(arg_ref(a, 1))?);
        let builtin = arg_z(a, 2);
        if builtin || cfg!(not(unix)) {
            t.set_field(library, IMPL, "handle", 1);
            return ret_z(true);
        }
        #[cfg(unix)]
        match t.vm.jni.load(t, &name) {
            Ok(handle) => {
                t.set_field(library, IMPL, "handle", handle as u64);
                t.set_field(library, IMPL, "jniVersion", 0x0018_0000);
                return ret_z(true);
            }
            Err(message) => {
                if arg_z(a, 3) {
                    return Err(t.throw_new("java/lang/UnsatisfiedLinkError", Some(&format!("{name}: {message}"))));
                }
                return ret_z(false);
            }
        }
        #[allow(unreachable_code)]
        ret_z(false)
    });
    n.add("jdk/internal/loader/NativeLibraries.unload(Ljava/lang/String;ZJ)V", no_op);
    n.add("jdk/internal/loader/NativeLibrary.findEntry0(JLjava/lang/String;)J", |t, a| {
        let handle = arg_j(a, 0) as usize;
        let name = t.vm.string_value(t.null_check(arg_ref(a, 2))?);
        #[cfg(unix)]
        if handle > 1 {
            return ret_j(crate::jni::symbol(handle, &name).unwrap_or(0) as i64);
        }
        let _ = (handle, name);
        ret_j(0)
    });
}

/// The JDK's native libraries, implemented by the VM itself.
const JDK_LIBRARIES: &[&str] = &[
    "java", "net", "nio", "zip", "jimage", "verify", "management", "management_ext", "prefs", "extnet", "sctp", "jaas",
    "j2pkcs11", "j2gss", "attach", "instrument", "rmi", "jsig", "syslookup", "fdlibm", "awt", "fontmanager", "lcms",
    "javajpeg", "mlib_image", "jsound", "awt_headless", "awt_xawt", "dt_socket", "jdwp",
];

fn module_define(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let module = t.null_check(arg_ref(a, 0))?;
    let packages = arg_ref(a, 4);
    let loader = t.field_ref(module, "java/lang/Module", "loader");
    let name = t.field_ref(module, "java/lang/Module", "name");
    let name = (!name.is_null()).then(|| t.vm.string_value(name));
    let count = if packages.is_null() { 0 } else { t.vm.heap.array_length(packages) as u32 };
    let package_names: Vec<String> = (0..count)
        .map(|i| {
            let package = Ref(t.vm.heap.read(packages, crate::heap::ARRAY_BASE_OFFSET + i * 4));
            t.vm.string_value(package).replace('.', "/")
        })
        .collect();
    t.vm.define_module(module, loader, name.as_deref(), &package_names);
    ret_void()
}
