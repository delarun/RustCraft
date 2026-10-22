//! A Java virtual machine for running Minecraft in the browser.
//!
//! Builds natively (for development and tests against a reference JVM) and,
//! later, for WebAssembly.

pub mod boot;
pub mod class;
pub mod classpath;
pub mod fs;
pub mod heap;
pub mod interp;
pub mod jit;
#[cfg(unix)]
pub mod jni;
pub mod memory;
mod loading;
pub mod native;
pub mod net;
mod pool;
mod records;
pub mod threads;
pub mod vm;
#[cfg(target_family = "wasm")]
mod web;

pub use boot::SystemConfig;
pub use classpath::{ClassPath, Source};
pub use heap::Ref;
pub use interp::{JResult, Thread, Throw};
pub use vm::{Vm, VmError, VmOptions};

use heap::ARRAY_BASE_OFFSET;

/// A hasher for small integer keys (object references, (method, pc)
/// pairs): the standard SipHash costs more than the lookups it serves.
#[derive(Default, Clone, Copy)]
pub struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }

    fn write_u16(&mut self, v: u16) {
        self.write_u64(v as u64);
    }

    fn write_u32(&mut self, v: u32) {
        self.write_u64(v as u64);
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
}

pub type FastMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FastHasher>>;

/// The processors the VM may use: $RJAVA_CPUS, else the host's (under WASI
/// the embedder passes the count, the platform reports one).
pub fn processors() -> usize {
    static CPUS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CPUS.get_or_init(|| {
        std::env::var("RJAVA_CPUS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
    })
}

/// Runs `main` of `main_class` (binary name) like the `java` launcher: boots
/// the class library, loads the class with the system class loader, runs
/// `main`, waits for non-daemon threads and runs shutdown hooks.
///
/// Returns the exit status (1 if `main` threw; the exception has then been
/// reported through the thread's uncaught-exception handler), or a
/// description of a failure to start the VM.
pub fn run_main(vm: &'static Vm, main_class: &str, args: &[String]) -> Result<i32, String> {
    let mut thread = Thread::new(vm);
    vm.threads.safepoints.enter();
    thread.attach();
    // RJAVA_WATCHDOG=<seconds>: periodically print every thread's Java stack.
    if let Some(seconds) = std::env::var("RJAVA_WATCHDOG").ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(seconds.max(1)));
            vm.threads.dump_stacks();
        });
    }
    let result = (|| -> JResult<Result<i32, String>> {
        if let Err(Throw(exception)) = thread.boot() {
            return Ok(Err(format!("Error occurred during initialization of boot layer\n{}", describe_throwable(&mut thread, exception))));
        }
        let status = match run_main_method(&mut thread, main_class, args) {
            Ok(()) => 0,
            Err(Throw(exception)) => {
                let java_thread = thread.java_thread;
                thread.invoke_virtual(java_thread, "dispatchUncaughtException", "(Ljava/lang/Throwable;)V", &[exception.slot()])?;
                1
            }
        };
        thread.destroy_vm()?;
        Ok(Ok(status))
    })();
    vm.print_profile();
    let result = result.unwrap_or_else(|Throw(exception)| Err(describe_throwable(&mut thread, exception)));
    thread.detach();
    vm.threads.safepoints.leave();
    result
}

fn run_main_method(thread: &mut Thread<'_>, main_class: &str, args: &[String]) -> JResult<()> {
    let loader = thread.system_class_loader()?;
    let class = thread.load_class_in(loader, &main_class.replace('.', "/"))?;
    thread.ensure_initialized(class)?;
    let main = class.find_method("main", "([Ljava/lang/String;)V").filter(|m| m.is_static()).ok_or_else(|| {
        thread.throw_new("java/lang/NoSuchMethodError", Some(&format!("{main_class}.main(String[])")))
    })?;
    let strings = args.iter().map(|a| thread.new_string(a)).collect::<JResult<Vec<_>>>()?;
    let array = thread.new_object_array("java/lang/String", &strings)?;
    thread.invoke(main, &[array.slot()])?;
    Ok(())
}

/// Formats a throwable like `Throwable.printStackTrace`, from VM data only
/// (usable before System.out exists).
pub fn describe_throwable(thread: &mut Thread<'_>, exception: Ref) -> String {
    let vm = thread.vm;
    let mut out = String::new();
    let mut current = exception;
    let mut depth = 0;
    while !current.is_null() && depth < 16 {
        let class = vm.class_of(current);
        let message = thread.field_ref(current, "java/lang/Throwable", "detailMessage");
        if depth > 0 {
            out.push_str("Caused by: ");
        }
        out.push_str(&class.java_name());
        if !message.is_null() {
            out.push_str(": ");
            out.push_str(&vm.string_value(message));
        }
        out.push('\n');
        let backtrace = thread.field_ref(current, "java/lang/Throwable", "backtrace");
        if !backtrace.is_null() && vm.class_of(backtrace).name == "[I" {
            let frames = vm.heap.array_length(backtrace) as u32 / 2;
            for i in 0..frames {
                let method = vm.method_by_index(vm.heap.read(backtrace, ARRAY_BASE_OFFSET + i * 8));
                let pc: u32 = vm.heap.read(backtrace, ARRAY_BASE_OFFSET + i * 8 + 4);
                let class = method.class();
                let file = class.file.as_ref().and_then(|f| f.source_file.clone()).unwrap_or_else(|| "Unknown Source".into());
                let location = match method.line_number(pc) {
                    _ if method.is_native() => "Native Method".to_owned(),
                    Some(line) => format!("{file}:{line}"),
                    None => file,
                };
                out.push_str(&format!("\tat {}.{}({location})\n", class.java_name(), method.name));
            }
        }
        let cause = thread.field_ref(current, "java/lang/Throwable", "cause");
        current = if cause == current { Ref::NULL } else { cause };
        depth += 1;
    }
    out
}
