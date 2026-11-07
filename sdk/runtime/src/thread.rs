// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Threads of this process (ADR 0082): a closure on a thread of its own, joined
//! for its value, and a pool of threads that run the parts of one job at a time,
//! as a renderer drawing a frame in bands does. A panic on any thread ends the
//! whole program, as on the main one.

use crate::sys::{self, abi::TIMEOUT_INFINITE, Error};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

type Job = Box<dyn FnOnce() -> u64 + Send>;

extern "C" fn start(arg: u64) -> ! {
    // SAFETY: `arg` is the box `spawn` gave this thread, and only it takes it.
    let job = unsafe { Box::from_raw(arg as *mut Job) };
    sys::thread_exit(job())
}

/// A thread [`spawn`] started, to [`join`](JoinHandle::join).
pub struct JoinHandle {
    tid: u64,
}

/// Runs `f` on a new thread of this process; [`Error::Limit`] past the process's
/// thread limit.
pub fn spawn(f: impl FnOnce() -> u64 + Send + 'static) -> Result<JoinHandle, Error> {
    let job: *mut Job = Box::into_raw(Box::new(Box::new(f)));
    match sys::thread_create(start, job as u64) {
        Ok(tid) => Ok(JoinHandle { tid }),
        Err(e) => {
            // SAFETY: the thread was not made, so the box is still ours alone.
            drop(unsafe { Box::from_raw(job) });
            Err(e)
        }
    }
}

impl JoinHandle {
    pub fn tid(&self) -> u64 {
        self.tid
    }

    /// Waits for the thread to end and gives what its closure returned.
    pub fn join(self) -> Result<u64, Error> {
        sys::thread_join(self.tid, TIMEOUT_INFINITE)
    }
}

/// What the pool's threads and its owner share.
struct Shared {
    /// One more for each job, and for the end: the pool's threads wait on it.
    round: AtomicU32,
    /// Pool threads yet to finish the job under way: its caller waits on it.
    left: AtomicU32,
    /// The job's parts.
    parts: AtomicU32,
    quit: AtomicBool,
    /// The caller's closure while [`Pool::run`] waits for it: written before
    /// `round` rises, read by a pool thread after it sees `round` rise and before
    /// it lowers `left`.
    job: UnsafeCell<Option<*const (dyn Fn(usize) + Sync)>>,
}

// SAFETY: `job` is only touched as described on it, ordered by `round` and `left`.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

/// Threads that run the parts of one job at a time: [`run`](Pool::run) calls
/// `f(0)` on the calling thread and `f(i)` on pool thread `i`.
pub struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle>,
}

impl Pool {
    /// A pool of `threads` threads besides the caller's; as many as the process's
    /// limit allows, which may be fewer (see [`size`](Pool::size)).
    pub fn new(threads: usize) -> Self {
        let shared = Arc::new(Shared {
            round: AtomicU32::new(0),
            left: AtomicU32::new(0),
            parts: AtomicU32::new(0),
            quit: AtomicBool::new(false),
            job: UnsafeCell::new(None),
        });
        let mut pool = Pool {
            shared,
            threads: Vec::new(),
        };
        for index in 1..=threads {
            let shared = pool.shared.clone();
            match spawn(move || serve(&shared, index)) {
                Ok(t) => pool.threads.push(t),
                Err(_) => break,
            }
        }
        pool
    }

    /// Parts that run at once: the pool's threads and the caller.
    pub fn size(&self) -> usize {
        self.threads.len() + 1
    }

    /// Runs `f(0)` to `f(parts - 1)` and returns when all have: part `i` on pool
    /// thread `i`, and part 0 and any beyond the pool on the caller.
    pub fn run(&self, parts: usize, f: &(dyn Fn(usize) + Sync)) {
        let s = &*self.shared;
        let n = self.threads.len();
        if n > 0 && parts > 1 {
            // SAFETY: the pool's threads read `job` only between the rise of
            // `round` below and their lowering of `left`, and this waits for all
            // of them to lower it before returning: `f` outlives every use.
            unsafe {
                let erased: *const (dyn Fn(usize) + Sync + '_) = f;
                *s.job.get() = Some(core::mem::transmute::<
                    *const (dyn Fn(usize) + Sync + '_),
                    *const (dyn Fn(usize) + Sync),
                >(erased));
            }
            s.parts.store(parts as u32, Ordering::Relaxed);
            s.left.store(n as u32, Ordering::Relaxed);
            s.round.fetch_add(1, Ordering::Release);
            sys::futex_wake(&s.round, n as u32);
        }
        f(0);
        for part in (n + 1)..parts {
            f(part);
        }
        if n > 0 && parts > 1 {
            loop {
                let left = s.left.load(Ordering::Acquire);
                if left == 0 {
                    break;
                }
                let _ = sys::futex_wait(&s.left, left, TIMEOUT_INFINITE);
            }
            // SAFETY: every pool thread is done with it.
            unsafe { *s.job.get() = None };
        }
    }
}

/// A pool thread: waits for each round and runs its part of the job.
fn serve(s: &Shared, index: usize) -> u64 {
    let mut seen = 0u32;
    loop {
        let round = s.round.load(Ordering::Acquire);
        if round == seen {
            let _ = sys::futex_wait(&s.round, round, TIMEOUT_INFINITE);
            continue;
        }
        seen = round;
        if s.quit.load(Ordering::Acquire) {
            return 0;
        }
        if index < s.parts.load(Ordering::Relaxed) as usize {
            // SAFETY: set before `round` rose, which this thread has seen, and
            // kept until it lowers `left`.
            if let Some(job) = unsafe { *s.job.get() } {
                // SAFETY: as above, the closure lives until `left` reaches 0.
                unsafe { (*job)(index) };
            }
        }
        if s.left.fetch_sub(1, Ordering::AcqRel) == 1 {
            sys::futex_wake(&s.left, 1);
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let s = &*self.shared;
        s.quit.store(true, Ordering::Release);
        s.round.fetch_add(1, Ordering::Release);
        sys::futex_wake(&s.round, self.threads.len() as u32);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
