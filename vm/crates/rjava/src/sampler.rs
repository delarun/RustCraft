//! RJAVA_CPUPROF: a leaf-only CPU sampler.
//!
//! An ITIMER_PROF timer sends SIGPROF for every millisecond of process CPU
//! time; the handler only stores the interrupted instruction address in a
//! preallocated buffer (no locks, no unwinding: safe in compiled wasm code,
//! in the allocator, anywhere). At exit the addresses are attributed to Rust
//! functions or, for JIT code, to the compiled Java method.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

const CAPACITY: usize = 1 << 22;

static SAMPLES: AtomicUsize = AtomicUsize::new(0);
static BUFFER: AtomicUsize = AtomicUsize::new(0); // *const AtomicUsize
static THREADS: AtomicUsize = AtomicUsize::new(0); // *const AtomicUsize: the sampled thread ids
/// Thread names by id, recorded while the threads run (they may be gone by
/// the report).
static NAMES: std::sync::Mutex<Option<std::collections::HashMap<usize, String>>> = std::sync::Mutex::new(None);

fn record_thread_names() {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else { return };
    let mut names = NAMES.lock().unwrap();
    let names = names.get_or_insert_with(Default::default);
    for task in tasks.flatten() {
        if let Ok(tid) = task.file_name().to_string_lossy().parse::<usize>() {
            if let Ok(name) = std::fs::read_to_string(task.path().join("comm")) {
                names.insert(tid, name.trim().to_owned());
            }
        }
    }
}

extern "C" fn on_sigprof(_: libc::c_int, _: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let buffer = BUFFER.load(Relaxed) as *const AtomicUsize;
    if buffer.is_null() {
        return;
    }
    // SAFETY: the kernel passes a ucontext_t; x86-64 Linux register layout.
    let pc = unsafe { (*(context as *const libc::ucontext_t)).uc_mcontext.gregs[libc::REG_RIP as usize] as usize };
    let index = SAMPLES.fetch_add(1, Relaxed);
    if index < CAPACITY {
        // SAFETY: within the buffers allocated in `start`; gettid is
        // async-signal-safe.
        unsafe {
            (*buffer.add(index)).store(pc, Relaxed);
            let tid = libc::syscall(libc::SYS_gettid) as usize;
            (*(THREADS.load(Relaxed) as *const AtomicUsize).add(index)).store(tid, Relaxed);
        }
    }
}

/// Starts sampling at `hz` samples per CPU second.
pub fn start(hz: u32) {
    let buffer: Box<[AtomicUsize]> = (0..CAPACITY).map(|_| AtomicUsize::new(0)).collect();
    let threads: Box<[AtomicUsize]> = (0..CAPACITY).map(|_| AtomicUsize::new(0)).collect();
    THREADS.store(Box::leak(threads).as_ptr() as usize, Relaxed);
    BUFFER.store(Box::leak(buffer).as_ptr() as usize, Relaxed);
    std::thread::spawn(|| loop {
        record_thread_names();
        std::thread::sleep(std::time::Duration::from_millis(200));
    });
    // SAFETY: installing a handler that only does atomic stores.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigprof as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGPROF, &action, std::ptr::null_mut());
        let interval = libc::timeval { tv_sec: 0, tv_usec: (1_000_000 / hz) as libc::suseconds_t };
        let timer = libc::itimerval { it_interval: interval, it_value: interval };
        libc::setitimer(libc::ITIMER_PROF, &timer, std::ptr::null_mut());
    }
}

/// Stops sampling and prints the functions with the most samples (overall,
/// then for the `focus` thread), and samples per thread.
/// `jit_method` names compiled code at an address, if it is any.
pub fn report(jit_method: &dyn Fn(usize) -> Option<String>, top: usize, focus: &str) {
    // SAFETY: disarming the timer.
    unsafe {
        let zero = libc::itimerval { it_interval: libc::timeval { tv_sec: 0, tv_usec: 0 }, it_value: libc::timeval { tv_sec: 0, tv_usec: 0 } };
        libc::setitimer(libc::ITIMER_PROF, &zero, std::ptr::null_mut());
    }
    let count = SAMPLES.load(Relaxed).min(CAPACITY);
    let buffer = BUFFER.load(Relaxed) as *const AtomicUsize;
    let threads = THREADS.load(Relaxed) as *const AtomicUsize;
    // SAFETY: within the buffers.
    let samples: Vec<(usize, usize)> =
        (0..count).map(|i| unsafe { ((*buffer.add(i)).load(Relaxed), (*threads.add(i)).load(Relaxed)) }).collect();
    record_thread_names();
    let recorded = NAMES.lock().unwrap().clone().unwrap_or_default();
    let name_of = |tid: usize| recorded.get(&tid).cloned().unwrap_or(format!("thread {tid}"));
    let mut names: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut per_thread: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for &(_, tid) in &samples {
        let name = names.entry(tid).or_insert_with(|| name_of(tid)).clone();
        *per_thread.entry(name).or_default() += 1;
    }
    let all: Vec<usize> = samples.iter().map(|&(pc, _)| pc).collect();
    print_top(&format!("self time ({count} samples)"), &all, jit_method, top);
    let focused: Vec<usize> = samples.iter().filter(|&&(_, tid)| names.get(&tid).is_some_and(|n| n == focus)).map(|&(pc, _)| pc).collect();
    if !focused.is_empty() {
        print_top(&format!("self time of \"{focus}\" ({} samples)", focused.len()), &focused, jit_method, top / 2);
    }
    let mut per_thread: Vec<(String, usize)> = per_thread.into_iter().collect();
    per_thread.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("--- samples per thread ---");
    for (name, n) in per_thread.iter().take(20) {
        eprintln!("{:5.1}%  {name}", *n as f64 * 100.0 / count.max(1) as f64);
    }
}

fn print_top(title: &str, pcs: &[usize], jit_method: &dyn Fn(usize) -> Option<String>, top: usize) {
    let count = pcs.len();
    let mut by_pc: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for &pc in pcs {
        *by_pc.entry(pc).or_default() += 1;
    }
    let mut by_name: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut jit_total = 0;
    let mut unknown: Vec<(usize, usize)> = Vec::new();
    for (pc, n) in by_pc {
        let name = match jit_method(pc) {
            Some(method) => {
                jit_total += n;
                format!("[jit] {method}")
            }
            None => {
                let mut name = None;
                backtrace::resolve(pc as *mut libc::c_void, |symbol| {
                    if name.is_none() {
                        name = symbol.name().map(|n| format!("{n:#}"));
                    }
                });
                if name.is_none() {
                    unknown.push((pc, n));
                }
                name.unwrap_or_else(|| "<unknown>".into())
            }
        };
        *by_name.entry(name).or_default() += n;
    }
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let mapping = |pc: usize| {
        maps.lines()
            .find(|line| {
                let range = line.split(' ').next().unwrap_or("");
                let (a, b) = range.split_once('-').unwrap_or(("0", "0"));
                let (a, b) = (usize::from_str_radix(a, 16).unwrap_or(0), usize::from_str_radix(b, 16).unwrap_or(0));
                a <= pc && pc < b
            })
            .map(|line| line.split_whitespace().skip(5).collect::<Vec<_>>().join(" "))
            .unwrap_or_else(|| "?".into())
    };
    let mut unknown_by_mapping: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (pc, n) in &unknown {
        *unknown_by_mapping.entry(mapping(*pc)).or_default() += n;
    }
    for (mapping, n) in unknown_by_mapping {
        by_name.insert(format!("<unknown> in {}", if mapping.is_empty() { "anonymous memory".into() } else { mapping }), n);
    }
    by_name.remove("<unknown>");
    let mut rows: Vec<(String, usize)> = by_name.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("--- {title}, {:.1}% in compiled code ---", jit_total as f64 * 100.0 / count.max(1) as f64);
    for (name, n) in rows.iter().take(top) {
        eprintln!("{:5.1}%  {name}", *n as f64 * 100.0 / count.max(1) as f64);
    }
}
