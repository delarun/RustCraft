//! `rjava [-cp <path>:<path>...] [-Xmx<size>] [-D<key>=<value>...] [--modules <jdk lib/modules>] <MainClass> [args...]`
//!
//! The JDK image defaults to $RJAVA_MODULES. Hot methods are compiled to
//! WebAssembly and run with wasmtime (in the browser: through the JS API)
//! unless RJAVA_NO_JIT is set.

use jvm::{ClassPath, Source, Vm, VmOptions};
use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod sampler;

fn main() -> ExitCode {
    #[cfg(target_family = "wasm")]
    reserve_memory();
    let mut args = std::env::args().skip(1);
    let mut classpath = Vec::new();
    let mut modules = std::env::var_os("RJAVA_MODULES").map(PathBuf::from);
    let mut heap_size = None;
    let mut properties: Vec<(String, String)> = Vec::new();
    let main_class = loop {
        match args.next().as_deref() {
            Some(arg) if arg.starts_with("-D") => {
                let (key, value) = arg[2..].split_once('=').unwrap_or((&arg[2..], ""));
                properties.push((key.to_owned(), value.to_owned()));
            }
            Some(arg) if arg.starts_with("-Xmx") => match parse_size(&arg[4..]) {
                Some(size) => heap_size = Some(size),
                None => {
                    eprintln!("rjava: invalid heap size {arg}");
                    return ExitCode::from(2);
                }
            },
            Some("-cp" | "-classpath" | "--class-path") => {
                classpath.extend(args.next().unwrap_or_default().split(':').filter(|p| !p.is_empty()).map(PathBuf::from))
            }
            Some("--modules") => modules = args.next().map(PathBuf::from),
            Some(class) => break class.to_owned(),
            None => {
                eprintln!("usage: rjava [-cp <path>] [--modules <lib/modules>] <MainClass> [args...]");
                return ExitCode::from(2);
            }
        }
    };
    let Some(modules) = modules else {
        eprintln!("rjava: no JDK image; pass --modules <jdk>/lib/modules or set RJAVA_MODULES");
        return ExitCode::from(2);
    };

    // Only the JDK image is on the boot class path; the application class path
    // is read by the system class loader through java.class.path.
    let mut path = ClassPath::default();
    match Source::open(&modules) {
        Ok(source) => path.push(source),
        Err(e) => {
            eprintln!("rjava: cannot open {}: {e}", modules.display());
            return ExitCode::from(2);
        }
    }
    let classpath: Vec<PathBuf> = classpath.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
    let mut options = VmOptions { classpath: path, ..VmOptions::default() };
    if let Some(size) = heap_size {
        options.heap_size = size;
    }
    // RJAVA_NO_JIT: interpret only.
    #[cfg(target_family = "wasm")]
    if std::env::var_os("RJAVA_NO_JIT").is_none() {
        options.jit = Some(Box::new(jit_web::WebJit::new()));
    }
    #[cfg(not(target_family = "wasm"))]
    if std::env::var_os("RJAVA_NO_JIT").is_none() {
        let size = jit_wasmtime::WasmtimeJit::memory_size(options.heap_size, options.max_threads);
        match jit_wasmtime::WasmtimeJit::new(size) {
            Ok(jit) => {
                options.memory = Some(jit.region());
                options.jit = Some(Box::new(jit));
            }
            Err(e) => eprintln!("rjava: JIT unavailable: {e}"),
        }
    }
    // The JDK image is lib/modules inside the runtime directory, mounted as java.home.
    if let Some(jdk) = modules.parent().and_then(|lib| lib.parent()) {
        options.vfs.mount(&options.config.java_home, std::sync::Arc::new(jvm::fs::HostFs::new(jdk)));
    }
    options.config.class_path = classpath.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(":");
    options.config.command = main_class.clone();
    options.config.properties.extend(properties);
    let vm = match Vm::new(options) {
        Ok(vm) => vm.leak(),
        Err(e) => {
            eprintln!("rjava: VM initialisation failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let program_args: Vec<String> = args.collect();
    jvm::threads::Threads::start_sampler(vm);
    // RJAVA_DUMP_EVERY=<seconds>: print every thread's Java stack periodically
    // (a stand-in for jstack where there are no signals).
    if let Some(seconds) = std::env::var("RJAVA_DUMP_EVERY").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(seconds));
            vm.threads.dump_stacks();
        });
    }
    // RJAVA_CPUPROF=1: sample the VM's CPU time; report the top functions
    // (compiled Java methods included) at exit.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let profiling = std::env::var_os("RJAVA_CPUPROF").is_some();
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    if profiling {
        sampler::start(1000);
    }
    // Compiled code nests direct calls on the native stack: run Java on a
    // thread with a big one (as the VM does for Java threads).
    let main = std::thread::Builder::new()
        .name("main".into())
        .stack_size(if cfg!(target_family = "wasm") { 16 << 20 } else { 256 << 20 })
        .spawn(move || jvm::run_main(vm, &main_class, &program_args))
        .expect("spawning the main thread");
    let result = main.join().unwrap_or_else(|_| Err("the main thread panicked\n".into()));
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    if profiling {
        let jit_method = |pc: usize| {
            let jit = vm.jit.as_ref()?;
            jit.method_at(pc).map(|index| format!("{:?}", vm.method_by_index(index)))
        };
        sampler::report(&jit_method, 60, "Server thread");
    }
    match result {
        Ok(status) => ExitCode::from(status as u8),
        Err(message) => {
            eprint!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// `512m`, `2g`, `65536k`, or bytes.
fn parse_size(text: &str) -> Option<usize> {
    let (digits, unit) = match text.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&text[..i], c.to_ascii_lowercase()),
        _ => (text, 'b'),
    };
    let value: u64 = digits.parse().ok()?;
    let size = match unit {
        'b' => value,
        'k' => value << 10,
        'm' => value << 20,
        'g' => value << 30,
        _ => return None,
    };
    // Heap offsets are 32-bit (and under wasm32 so is usize).
    (size < 4 << 30).then_some(size as usize)
}

/// Under wasm, grows the memory once, up front, to $RJAVA_MEMORY_MB (default
/// 3584) and hands it to the allocator. Growing a shared memory while other
/// threads run is not safe in browsers: V8 tells other workers about the new
/// size asynchronously, and their accesses to it trap meanwhile. Pages are
/// only committed when touched.
#[cfg(target_family = "wasm")]
fn reserve_memory() {
    let mb: usize = std::env::var("RJAVA_MEMORY_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(3584);
    let current = core::arch::wasm32::memory_size(0) * 65536;
    let wanted = (mb << 20).saturating_sub(current);
    // Blocks of at most 1 GiB (allocation sizes are limited to isize::MAX).
    const BLOCK: usize = 1 << 30;
    let mut blocks = Vec::new();
    let mut left = wanted;
    while left > 0 {
        let size = left.min(BLOCK);
        let layout = std::alloc::Layout::from_size_align(size, 16).unwrap();
        // SAFETY: a non-zero layout; freed below with the same layout.
        let block = unsafe { std::alloc::alloc(layout) };
        if block.is_null() {
            break;
        }
        blocks.push((block, layout));
        left -= size;
    }
    for (block, layout) in blocks {
        // SAFETY: allocated above.
        unsafe { std::alloc::dealloc(block, layout) };
    }
}
