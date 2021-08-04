//! An interface for dealing with the kinds of parallel computations involved in
//! `bellperson`. It's currently just a thin wrapper around [`CpuPool`] and
//! [`rayon`] but may be extended in the future to allow for various
//! parallelism strategies.
//!
//! [`CpuPool`]: futures_cpupool::CpuPool

use crossbeam_channel::{bounded, Receiver};
use lazy_static::lazy_static;
use log::{error, trace};
use std::env;

use std::sync::atomic::{AtomicUsize, Ordering};

static WORKER_SPAWN_COUNTER: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    static ref NUM_CPUS: usize = if let Ok(num) = env::var("BELLMAN_NUM_CPUS") {
        if let Ok(num) = num.parse() {
            num
        } else {
            num_cpus::get()
        }
    } else {
        num_cpus::get()
    };
    // See Worker::compute below for a description of this.
    static ref WORKER_SPAWN_MAX_COUNT: usize = *NUM_CPUS * 4;
    pub static ref THREAD_POOL: rayon::ThreadPool = rayon::ThreadPoolBuilder::new()
        .num_threads(*NUM_CPUS)
        .build()
        .unwrap();
    pub static ref VERIFIER_POOL: rayon::ThreadPool = rayon::ThreadPoolBuilder::new()
        .num_threads(NUM_CPUS.max(6))
        .build()
        .unwrap();
}

#[derive(Clone)]
pub struct Worker {}

impl Worker {
    pub fn new() -> Worker {
        Worker {}
    }

    pub fn log_num_cpus(&self) -> u32 {
        log2_floor(*NUM_CPUS)
    }

    pub fn compute<F, R>(&self, f: F) -> Waiter<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (sender, receiver) = bounded(1);

        let thread_index = if THREAD_POOL.current_thread_index().is_some() {
            THREAD_POOL.current_thread_index().unwrap()
        } else {
            0
        };

        // We keep track here of how many times spawn has been called.
        // It can be called without limit, each time, putting a
        // request for a new thread to execute a method on the
        // ThreadPool.  However, if we allow it to be called without
        // limits, we run the risk of memory exhaustion due to limited
        // stack space consumed by all of the pending closures to be
        // executed.
        let previous_count = WORKER_SPAWN_COUNTER.fetch_add(1, Ordering::SeqCst);

        // If the number of spawns requested has exceeded the number
        // of cores available for processing by some factor (the
        // default being 4), instead of requesting that we spawn a new
        // thread, we instead execute the closure in the context of an
        // install call to help clear the growing work queue and
        // minimize the chances of memory exhaustion.
        if previous_count > *WORKER_SPAWN_MAX_COUNT {
            THREAD_POOL.install(move || {
                trace!("[{}] switching to install to help clear backlog[current threads {}, threads requested {}]",
                       thread_index,
                       THREAD_POOL.current_num_threads(),
                       WORKER_SPAWN_COUNTER.load(Ordering::SeqCst));
                let res = f();
                sender.send(res).unwrap();
                WORKER_SPAWN_COUNTER.fetch_sub(1, Ordering::SeqCst);
            });
        } else {
            THREAD_POOL.spawn(move || {
                let res = f();
                sender.send(res).unwrap();
                WORKER_SPAWN_COUNTER.fetch_sub(1, Ordering::SeqCst);
            });
        }

        Waiter { receiver }
    }

    pub fn scope<'a, F, R>(&self, elements: usize, f: F) -> R
    where
        F: FnOnce(&rayon::Scope<'a>, usize) -> R + Send,
        R: Send,
    {
        let chunk_size = if elements < *NUM_CPUS {
            1
        } else {
            elements / *NUM_CPUS
        };

        THREAD_POOL.scope(|scope| f(scope, chunk_size))
    }
}

pub struct Waiter<T> {
    receiver: Receiver<T>,
}

impl<T> Waiter<T> {
    /// Wait for the result.
    pub fn wait(&self) -> T {
        if THREAD_POOL.current_thread_index().is_some() {
            // Calling `wait()` from within the worker thread pool can lead to dead logs
            error!("The wait call should never be done inside the worker thread pool");
            debug_assert!(false);
        }
        self.receiver.recv().unwrap()
    }

    /// One off sending.
    pub fn done(val: T) -> Self {
        let (sender, receiver) = bounded(1);
        sender.send(val).unwrap();

        Waiter { receiver }
    }
}

fn log2_floor(num: usize) -> u32 {
    assert!(num > 0);

    let mut pow = 0;

    while (1 << (pow + 1)) <= num {
        pow += 1;
    }

    pow
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_log2_floor() {
        assert_eq!(log2_floor(1), 0);
        assert_eq!(log2_floor(3), 1);
        assert_eq!(log2_floor(4), 2);
        assert_eq!(log2_floor(5), 2);
        assert_eq!(log2_floor(6), 2);
        assert_eq!(log2_floor(7), 2);
        assert_eq!(log2_floor(8), 3);
    }
}
