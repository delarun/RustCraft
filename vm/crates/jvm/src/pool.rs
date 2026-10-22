//! Helper threads for the collector's parallel phases. They are started once
//! and then wait for work: in the browser a thread is a Web Worker, far too
//! slow to start for every collection.

use std::sync::{Condvar, Mutex};

struct State {
    /// The running job (borrowed from `run`, which waits until it is done).
    job: Option<&'static (dyn Fn(usize) + Sync)>,
    /// Incremented for every job.
    generation: u64,
    /// Helpers the job wants, helpers that took an index, and helpers still running it.
    wanted: usize,
    claimed: usize,
    running: usize,
    panicked: bool,
    /// Helper threads started.
    helpers: usize,
}

static STATE: Mutex<State> =
    Mutex::new(State { job: None, generation: 0, wanted: 0, claimed: 0, running: 0, panicked: false, helpers: 0 });
static START: Condvar = Condvar::new();
static DONE: Condvar = Condvar::new();

/// Calls `f(i)` for every `i` in `0..threads` in parallel (`f(0)` on the
/// calling thread) and returns when all calls have. One job at a time.
pub fn run(threads: usize, f: impl Fn(usize) + Sync) {
    if threads <= 1 {
        f(0);
        return;
    }
    let job: &(dyn Fn(usize) + Sync) = &f;
    // SAFETY: the job is cleared before this function returns, after every
    // helper finished calling it.
    let job: &'static (dyn Fn(usize) + Sync) = unsafe { std::mem::transmute(job) };
    {
        let mut state = STATE.lock().unwrap();
        while state.helpers < threads - 1 {
            state.helpers += 1;
            std::thread::Builder::new().name("rjava gc".into()).spawn(helper).expect("collector thread");
        }
        state.job = Some(job);
        state.generation += 1;
        state.wanted = threads - 1;
        state.claimed = 0;
        state.running = threads - 1;
        START.notify_all();
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(0)));
    let mut state = STATE.lock().unwrap();
    while state.running > 0 {
        state = DONE.wait(state).unwrap();
    }
    state.job = None;
    let panicked = std::mem::take(&mut state.panicked);
    drop(state);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(!panicked, "collector thread panicked");
}

fn helper() {
    let mut seen = 0;
    let mut state = STATE.lock().unwrap();
    loop {
        while state.generation == seen {
            state = START.wait(state).unwrap();
        }
        seen = state.generation;
        if state.claimed == state.wanted {
            continue;
        }
        state.claimed += 1;
        let index = state.claimed;
        let job = state.job.unwrap();
        drop(state);
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(index))).is_ok();
        state = STATE.lock().unwrap();
        state.panicked |= !ok;
        state.running -= 1;
        if state.running == 0 {
            DONE.notify_all();
        }
    }
}
