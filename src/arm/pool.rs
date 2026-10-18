//! The page's worker threads, for the cores to run on in the browser.
//!
//! WebAssembly has no `std::thread`: a page starts Web Workers itself, each
//! loads this module on the shared memory and calls [`run_forever`], which
//! takes jobs off one queue. [`submit`] is how a core's thread starts, and a
//! worker that has finished a job goes back for the next. The page starts them
//! before the first run (a worker's script loads only while its creator's event
//! loop turns, and the emulator's thread then blocks for a whole slice at a
//! time).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

type Job = Box<dyn FnOnce() + Send>;

struct Pool {
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
    threads: AtomicUsize,
}

static POOL: Pool = Pool {
    jobs: Mutex::new(VecDeque::new()),
    ready: Condvar::new(),
    threads: AtomicUsize::new(0),
};

/// Hand a job to the next free worker.
pub fn submit(job: Job) {
    POOL.jobs.lock().unwrap().push_back(job);
    POOL.ready.notify_one();
}

/// How many workers have joined the pool.
pub fn threads() -> usize {
    POOL.threads.load(Ordering::Acquire)
}

/// A worker's life: join the pool and run what comes, for good.
pub fn run_forever() {
    POOL.threads.fetch_add(1, Ordering::AcqRel);
    loop {
        let job = {
            let mut jobs = POOL.jobs.lock().unwrap();
            loop {
                if let Some(job) = jobs.pop_front() {
                    break job;
                }
                jobs = POOL.ready.wait(jobs).unwrap();
            }
        };
        job();
    }
}
