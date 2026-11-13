// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! Expander-level persistent pinned worker pool and sense-reversing barrier.
//!
//! Provides:
//! 1. [`SenseReversingBarrier`]: Reusable barrier using an atomic generation counter and release-acquire
//!    ordering to coordinate worker threads between expander graph levels without allocation or spin thrashing.
//! 2. Pinning and affinity discovery: [`pin_current_thread_to_core`], [`get_affinity_cores`], [`parse_core_list`].
//!    Respects process affinity masks (never implicitly CPU 2 on reserved hosts).
//! 3. [`ThreadPool`]: Persistent pinned worker threads that remain alive across all slices, recursion levels,
//!    and benchmark repetitions, parking gracefully via condition variables when idle.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Partition `total` items among `num_threads` into contiguous, disjoint ranges.
///
/// Remainder items are distributed evenly among the first `total % num_threads` partitions,
/// ensuring partition sizes differ by at most 1.
#[inline]
pub fn split_range(total: usize, tid: usize, num_threads: usize) -> (usize, usize) {
    assert!(tid < num_threads, "tid {tid} >= num_threads {num_threads}");
    let base = total / num_threads;
    let rem = total % num_threads;
    let start = tid * base + tid.min(rem);
    let count = base + if tid < rem { 1 } else { 0 };
    (start, start + count)
}

/// A reusable sense-reversing generation barrier for coordinating worker threads between expander levels.
///
/// # Memory Ordering Rationale
///
/// 1. **Arrival Phase (`count` decrement via `Ordering::AcqRel`)**:
///    - `Release` ensures that all memory writes performed by this thread prior to calling `wait()`
///      are published and visible to whatever thread observes `count == 1`.
///    - `Acquire` ensures any memory operations released by earlier operations are acquired.
///
/// 2. **Turnaround Phase (last thread via `Ordering::Release`)**:
///    - The last arriving thread observes `fetch_sub(1, Ordering::AcqRel) == 1`.
///    - By transitivity of `AcqRel` on `count`, the last thread has acquired all preceding memory
///      writes from all other `num_threads - 1` worker threads.
///    - The last thread resets `count` to `num_threads` with `Ordering::Release`.
///    - The last thread increments `generation` with `Ordering::Release`.
///
/// 3. **Departure Phase (waiting threads via `Ordering::Acquire`)**:
///    - Waiting threads observe the incremented `generation` using `Ordering::Acquire`.
///    - The `Acquire` load synchronizes-with the `Release` write to `generation` by the last thread.
///    - By the Release-Acquire sequence, all memory writes made by ANY thread before calling `wait()`
///      are guaranteed to happen-before ANY memory operations performed by ANY thread after returning
///      from `wait()`.
///
/// 4. **Waiting Strategy**:
///    - Spins with `std::hint::spin_loop()` for a small bound (64 iterations) to minimize latency
///      when threads arrive nearly simultaneously.
///    - If the generation has not yet advanced after 64 spins, yields the CPU time slice via
///      `std::thread::yield_now()`. This is essential on systems where multiple worker threads
///      share a logical core (such as T=8 workers on the 7 available cores of the local workstation),
///      preventing priority inversion and thread starvation.
pub struct SenseReversingBarrier {
    num_threads: usize,
    count: AtomicUsize,
    generation: AtomicUsize,
}

impl SenseReversingBarrier {
    /// Create a new barrier for `num_threads` threads.
    pub fn new(num_threads: usize) -> Self {
        assert!(num_threads > 0, "barrier must have at least 1 thread");
        Self {
            num_threads,
            count: AtomicUsize::new(num_threads),
            generation: AtomicUsize::new(0),
        }
    }

    /// Number of threads synchronized by this barrier.
    #[inline]
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }

    /// Synchronize all threads at this barrier.
    ///
    /// Blocks until all `num_threads` have called `wait()`.
    #[inline]
    pub fn wait(&self) {
        if self.num_threads <= 1 {
            return;
        }

        let gen = self.generation.load(Ordering::Acquire);
        let prev = self.count.fetch_sub(1, Ordering::AcqRel);

        if prev == 1 {
            // Last thread to arrive at this generation
            self.count.store(self.num_threads, Ordering::Release);
            self.generation.fetch_add(1, Ordering::Release);
        } else {
            // Wait for generation to advance
            let mut spins = 0usize;
            while self.generation.load(Ordering::Acquire) == gen {
                if spins < 64 {
                    std::hint::spin_loop();
                    spins += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        }
    }
}

/// Pin the calling thread to a specific CPU core ID.
///
/// On Linux, uses `libc::sched_setaffinity`.
/// On non-Linux platforms, or if `sched_setaffinity` returns an error (e.g. `EPERM`
/// in restricted containers), this gracefully degrades to an unpinned state with a logged warning.
pub fn pin_current_thread_to_core(core_id: usize) -> Result<(), std::io::Error> {
    #[cfg(target_os = "linux")]
    unsafe {
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(core_id, &mut cpuset);
        let ret = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            eprintln!("Warning: sched_setaffinity to core {core_id} failed: {err}");
            return Err(err);
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = core_id;
        Ok(())
    }
}

/// Query the CPU cores currently permitted by the calling process's affinity mask.
///
/// On Linux, queries `libc::sched_getaffinity`.
/// On non-Linux platforms, falls back to `std::thread::available_parallelism()`.
pub fn get_affinity_cores() -> Vec<usize> {
    #[cfg(target_os = "linux")]
    unsafe {
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut cpuset) == 0 {
            let mut cores = Vec::new();
            for i in 0..libc::CPU_SETSIZE as usize {
                if libc::CPU_ISSET(i, &cpuset) {
                    cores.push(i);
                }
            }
            if !cores.is_empty() {
                return cores;
            }
        }
    }

    let count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    (0..count).collect()
}

/// Parse a comma-separated or range-based core list string like `"0,1,3-7"` into `Vec<usize>`.
pub fn parse_core_list(s: &str) -> Result<Vec<usize>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(get_affinity_cores());
    }

    let mut cores = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start_s, end_s)) = part.split_once('-') {
            let start: usize = start_s
                .trim()
                .parse()
                .map_err(|e| format!("invalid core range start '{start_s}': {e}"))?;
            let end: usize = end_s
                .trim()
                .parse()
                .map_err(|e| format!("invalid core range end '{end_s}': {e}"))?;
            if start > end {
                return Err(format!("invalid core range: start {start} > end {end}"));
            }
            for c in start..=end {
                cores.push(c);
            }
        } else {
            let c: usize = part
                .parse()
                .map_err(|e| format!("invalid core '{part}': {e}"))?;
            cores.push(c);
        }
    }

    if cores.is_empty() {
        return Err("empty core list".to_string());
    }
    cores.sort_unstable();
    cores.dedup();
    Ok(cores)
}

/// Trait representing a collaborative job executed across worker threads in a [`ThreadPool`].
pub trait WorkerJob: Send + Sync {
    /// Execute worker `tid`'s share of the job.
    fn run(&self, tid: usize, num_threads: usize, barrier: &SenseReversingBarrier);
}

#[derive(Clone, Copy)]
struct JobInvocation {
    job_id: usize,
    data: *const (),
    run_fn: unsafe fn(*const (), usize, usize, &SenseReversingBarrier),
}

unsafe impl Send for JobInvocation {}
unsafe impl Sync for JobInvocation {}

struct WorkerState {
    next_job_id: AtomicUsize,
    job: Mutex<Option<JobInvocation>>,
    job_cvar: Condvar,
    shutdown: AtomicBool,
    barrier: SenseReversingBarrier,
    done_count: AtomicUsize,
    done_mutex: Mutex<bool>,
    done_cvar: Condvar,
}

// Safety: the pointer to WorkerJob is only dereferenced while the coordinator
// thread is blocked inside `ThreadPool::execute`, guaranteeing the job reference remains valid.
unsafe impl Send for WorkerState {}
unsafe impl Sync for WorkerState {}

/// Persistent pinned worker thread pool for expander-level multithreading.
///
/// Spawns worker threads once during initialization and preserves them across all slices,
/// recursion levels, and benchmark repetitions. When idle, workers park via condition variables
/// rather than burning CPU cycles.
pub struct ThreadPool {
    num_threads: usize,
    cores: Vec<usize>,
    shared: Arc<WorkerState>,
    workers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for ThreadPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadPool")
            .field("num_threads", &self.num_threads)
            .field("cores", &self.cores)
            .finish()
    }
}

impl ThreadPool {
    /// Create a new persistent pinned worker pool with `num_threads` worker threads.
    ///
    /// If `cores` is empty, available cores are detected from the process affinity mask.
    /// Worker `i` is pinned to core `cores[i % cores.len()]`.
    pub fn new(num_threads: usize, cores: &[usize]) -> Self {
        assert!(num_threads > 0, "ThreadPool must have at least 1 thread");
        let core_list = if cores.is_empty() {
            get_affinity_cores()
        } else {
            cores.to_vec()
        };

        let shared = Arc::new(WorkerState {
            next_job_id: AtomicUsize::new(1),
            job: Mutex::new(None),
            job_cvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            barrier: SenseReversingBarrier::new(num_threads),
            done_count: AtomicUsize::new(num_threads),
            done_mutex: Mutex::new(false),
            done_cvar: Condvar::new(),
        });

        let mut workers = Vec::with_capacity(num_threads);
        for tid in 0..num_threads {
            let shared_worker = Arc::clone(&shared);
            let core = core_list[tid % core_list.len()];
            let handle = std::thread::Builder::new()
                .name(format!("lcpc-worker-{tid}"))
                .spawn(move || {
                    let _ = pin_current_thread_to_core(core);
                    worker_loop(tid, num_threads, shared_worker);
                })
                .expect("failed to spawn worker thread");
            workers.push(handle);
        }

        Self {
            num_threads,
            cores: core_list,
            shared,
            workers,
        }
    }

    /// Number of worker threads in the pool.
    #[inline]
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }

    /// CPU cores assigned to this pool.
    #[inline]
    pub fn cores(&self) -> &[usize] {
        &self.cores
    }

    /// Execute a job across all worker threads collaboratively.
    ///
    /// Blocks until all worker threads have completed their portion of the job.
    pub fn execute<J: WorkerJob>(&self, job: &J) {
        unsafe fn trampoline<J: WorkerJob>(
            ptr: *const (),
            tid: usize,
            num_threads: usize,
            barrier: &SenseReversingBarrier,
        ) {
            let job = &*(ptr as *const J);
            job.run(tid, num_threads, barrier);
        }

        let new_job_id = self.shared.next_job_id.fetch_add(1, Ordering::SeqCst);
        let invocation = JobInvocation {
            job_id: new_job_id,
            data: job as *const J as *const (),
            run_fn: trampoline::<J>,
        };

        {
            let mut done = self.shared.done_mutex.lock().unwrap();
            *done = false;
            self.shared.done_count.store(self.num_threads, Ordering::Release);
            let mut job_lock = self.shared.job.lock().unwrap();
            *job_lock = Some(invocation);
            self.shared.job_cvar.notify_all();
        }

        // Wait for all workers to complete
        let mut done = self.shared.done_mutex.lock().unwrap();
        while !*done {
            done = self.shared.done_cvar.wait(done).unwrap();
        }
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.job_cvar.notify_all();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

fn worker_loop(tid: usize, num_threads: usize, shared: Arc<WorkerState>) {
    let mut my_job_id = 0usize;
    loop {
        let invocation = {
            let mut lock = shared.job.lock().unwrap();
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if let Some(inv) = *lock {
                    if inv.job_id > my_job_id {
                        my_job_id = inv.job_id;
                        break inv;
                    }
                }
                lock = shared.job_cvar.wait(lock).unwrap();
            }
        };

        // Execute the job collaboratively
        // Safety: the pointer is valid and coordinator blocks until done_cvar is signaled
        unsafe {
            (invocation.run_fn)(invocation.data, tid, num_threads, &shared.barrier);
        }

        // Barrier after job execution to ensure all workers have finished using invocation.data
        // before the coordinator is notified and returns.
        shared.barrier.wait();

        // Signal completion
        if shared.done_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut done = shared.done_mutex.lock().unwrap();
            *done = true;
            shared.done_cvar.notify_one();
        }
    }
}
