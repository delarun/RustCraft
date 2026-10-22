//! jdk.internal.util.SystemProps$Raw: the VM- and platform-provided system
//! properties read by System.initPhase1.

use super::*;
use classfile::Constant;

const RAW: &str = "jdk/internal/util/SystemProps$Raw";

pub(super) fn register(n: &mut Natives) {
    n.add("jdk/internal/util/SystemProps$Raw.vmProperties()[Ljava/lang/String;", vm_properties);
    n.add("jdk/internal/util/SystemProps$Raw.platformProperties()[Ljava/lang/String;", platform_properties);
}

/// Key/value pairs, flattened: VM properties, then `-D` properties.
fn vm_properties(t: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    let config = &t.vm.config;
    let mut pairs: Vec<(String, String)> = vec![
        ("java.home".into(), config.java_home.clone()),
        ("java.vm.specification.name".into(), "Java Virtual Machine Specification".into()),
        ("java.vm.specification.vendor".into(), "Oracle Corporation".into()),
        ("java.vm.specification.version".into(), "25".into()),
        ("java.vm.name".into(), "MinecraftWeb VM".into()),
        ("java.vm.vendor".into(), "MinecraftWeb".into()),
        ("java.vm.version".into(), env!("CARGO_PKG_VERSION").into()),
        ("java.vm.info".into(), "interpreted mode".into()),
        ("jdk.debug".into(), "release".into()),
        ("sun.boot.library.path".into(), format!("{}/lib", config.java_home)),
        ("java.library.path".into(), String::new()),
        ("java.class.path".into(), config.class_path.clone()),
        ("sun.java.launcher".into(), "SUN_STANDARD".into()),
        ("sun.nio.MaxDirectMemorySize".into(), "-1".into()),
        ("sun.nio.PageAlignDirectMemory".into(), "false".into()),
        ("sun.java.command".into(), config.command.clone()),
        // The VM has no epoll; select with poll(2) semantics (see native/socket.rs).
        ("java.nio.channels.spi.SelectorProvider".into(), "sun.nio.ch.PollSelectorProvider".into()),
    ];
    pairs.extend(config.properties.iter().cloned());
    let mut strings = Vec::with_capacity(pairs.len() * 2);
    for (key, value) in pairs {
        strings.push(t.new_string(&key)?);
        strings.push(t.new_string(&value)?);
    }
    ret_ref(t.new_object_array("java/lang/String", &strings)?)
}

/// A String[FIXED_LENGTH] indexed by the `_xxx_NDX` constants of SystemProps$Raw.
fn platform_properties(t: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    let raw = t.load_class(RAW)?;
    let constant = |name: &str| -> usize {
        let field = raw.find_field(name, "I").unwrap_or_else(|| panic!("{RAW}.{name}"));
        match raw.cp().get(field.constant_value.expect("constant field")) {
            Ok(Constant::Integer(v)) => *v as usize,
            other => panic!("{RAW}.{name}: unexpected constant {other:?}"),
        }
    };
    let config = &t.vm.config;
    let pointer_bits = (std::mem::size_of::<usize>() * 8).to_string();
    let values: Vec<(&str, String)> = vec![
        ("_display_language_NDX", "en".into()),
        ("_display_country_NDX", "US".into()),
        ("_format_language_NDX", "en".into()),
        ("_format_country_NDX", "US".into()),
        ("_file_separator_NDX", "/".into()),
        ("_path_separator_NDX", ":".into()),
        ("_line_separator_NDX", "\n".into()),
        ("_java_io_tmpdir_NDX", config.tmp_dir.clone()),
        ("_native_encoding_NDX", "UTF-8".into()),
        ("_sun_jnu_encoding_NDX", "UTF-8".into()),
        ("_stdout_encoding_NDX", "UTF-8".into()),
        ("_stderr_encoding_NDX", "UTF-8".into()),
        ("_os_name_NDX", config.os_name.clone()),
        ("_os_arch_NDX", config.os_arch.clone()),
        ("_os_version_NDX", config.os_version.clone()),
        ("_sun_arch_data_model_NDX", pointer_bits),
        ("_sun_cpu_endian_NDX", "little".into()),
        ("_sun_io_unicode_encoding_NDX", "UnicodeLittle".into()),
        ("_user_dir_NDX", config.user_dir.clone()),
        ("_user_home_NDX", config.user_home.clone()),
        ("_user_name_NDX", config.user_name.clone()),
    ];
    let mut items = vec![Ref::NULL; constant("FIXED_LENGTH")];
    for (index_name, value) in values {
        items[constant(index_name)] = t.new_string(&value)?;
    }
    ret_ref(t.new_object_array("java/lang/String", &items)?)
}
