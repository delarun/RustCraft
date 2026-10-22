//! Compiles the Java programs in tests/java, runs each on a reference JVM and
//! on our VM (interpreted, and with the JIT compiling everything warm), and
//! compares their output. Needs `javac`/`java` on PATH (any version ≥ 25) and
//! the JDK 25 image in .scratch; skipped otherwise.

use jvm::{ClassPath, Source, Vm, VmOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn modules() -> Option<PathBuf> {
    let path = manifest_dir().join("../../../.scratch/jre25/lib/modules");
    path.exists().then_some(path)
}

/// Compiled test classes (once per test binary run).
fn classes() -> Option<&'static Path> {
    static CLASSES: OnceLock<Option<PathBuf>> = OnceLock::new();
    CLASSES
        .get_or_init(|| {
            let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("java-tests");
            let _ = std::fs::remove_dir_all(&out);
            let sources: Vec<PathBuf> = walk(&manifest_dir().join("tests/java"));
            let status = Command::new("javac")
                .args(["--release", "25", "-XDstringConcat=inline", "-g", "-d"])
                .arg(&out)
                .args(&sources)
                .status()
                .ok()?;
            assert!(status.success(), "javac failed");
            Some(out)
        })
        .as_deref()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else if path.extension().is_some_and(|e| e == "java") {
            files.push(path);
        }
    }
    files
}

struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_ours(classes: &Path, modules: &Path, main: &str, heap_size: usize, jit: bool) -> (String, Result<i32, String>) {
    let mut path = ClassPath::default();
    path.push(Source::open(modules).unwrap());
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let mut options = VmOptions {
        heap_size,
        classpath: path,
        stdout: Box::new(Capture(stdout.clone())),
        ..VmOptions::default()
    };
    let jdk = modules.parent().unwrap().parent().unwrap();
    options.vfs.mount("/jdk", std::sync::Arc::new(jvm::fs::HostFs::new(jdk)));
    options.config.class_path = classes.display().to_string();
    if jit {
        // Compile methods after a few calls, so tests exercise compiled code.
        // SAFETY: every JIT test sets the same value.
        unsafe {
            std::env::set_var("RJAVA_JIT_THRESHOLD", "5");
            // Compile on the calling thread, so the code that follows runs compiled.
            std::env::set_var("RJAVA_JIT_SYNC", "1");
        }
        let backend = jit_wasmtime::WasmtimeJit::new(jit_wasmtime::WasmtimeJit::memory_size(heap_size, options.max_threads))
            .expect("JIT backend");
        options.memory = Some(backend.region());
        options.jit = Some(Box::new(backend));
    }
    let vm = Vm::new(options).expect("VM starts").leak();
    // Compiled code nests calls on the native stack.
    let main = main.to_owned();
    let result = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || jvm::run_main(vm, &main, &[]))
        .unwrap()
        .join()
        .unwrap_or_else(|_| Err("main thread panicked".into()));
    let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
    (output, result)
}

fn run_reference(classes: &Path, main: &str) -> String {
    let output = Command::new("java").arg("-cp").arg(classes).arg(main).output().expect("java runs");
    assert!(output.status.success(), "reference JVM failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

fn check(main: &str, jit: bool) {
    check_with_heap(main, 256 << 20, jit)
}

fn check_with_heap(main: &str, heap_size: usize, jit: bool) {
    let (Some(modules), Some(classes)) = (modules(), classes()) else {
        eprintln!("skipping {main}: JDK image or javac missing");
        return;
    };
    // FilesApi works in a fixed directory: one run at a time.
    static FILES: Mutex<()> = Mutex::new(());
    let _guard = (main == "FilesApi").then(|| FILES.lock().unwrap_or_else(|e| e.into_inner()));
    let expected = run_reference(classes, main);
    let (actual, result) = run_ours(classes, &modules, main, heap_size, jit);
    match &result {
        Ok(0) => {}
        Ok(status) => panic!("{main} exited with {status} on our VM\n--- output ---\n{actual}"),
        Err(message) => panic!("{main} failed to start on our VM:\n{message}\n--- output so far ---\n{actual}"),
    }
    if actual != expected {
        let diff: Vec<String> = expected
            .lines()
            .zip(actual.lines().chain(std::iter::repeat("<missing>")))
            .filter(|(e, a)| e != a)
            .map(|(e, a)| format!("expected: {e}\n  actual: {a}"))
            .collect();
        panic!("{main}: output differs\n{}", diff.join("\n"));
    }
}

#[test]
fn basics() {
    check("Basics", false);
}

#[test]
fn threads() {
    check("Threads", false);
}

#[test]
fn gc_stress() {
    // A small heap forces many collections during the run.
    check_with_heap("GcStress", 96 << 20, false);
}

#[test]
fn sockets() {
    check("Sockets", false);
}

#[test]
fn files_api() {
    check("FilesApi", false);
}

#[test]
fn jit_patterns() {
    check("JitPatterns", false);
}

#[test]
fn jit_patterns_jit() {
    check("JitPatterns", true);
}

#[test]
fn basics_jit() {
    check("Basics", true);
}

#[test]
fn threads_jit() {
    check("Threads", true);
}

#[test]
fn gc_stress_jit() {
    check_with_heap("GcStress", 96 << 20, true);
}

#[test]
fn sockets_jit() {
    check("Sockets", true);
}

#[test]
fn files_api_jit() {
    check("FilesApi", true);
}

#[test]
fn records() {
    check("Records", false);
}

#[test]
fn records_jit() {
    check("Records", true);
}

#[test]
fn ops() {
    check("Ops", false);
}

#[test]
fn ops_jit() {
    check("Ops", true);
}

#[test]
fn pure_natives() {
    check("PureNatives", false);
}

#[test]
fn pure_natives_jit() {
    check("PureNatives", true);
}
