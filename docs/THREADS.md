<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 08: Expander-Level Multithreading, Persistent Pinned Worker Pool & Barrier (`step/08-threads`)

This document describes the design, algorithmic formulation, barrier memory ordering, microarchitectural behavior, and verification of **expander-level multithreading with a persistent pinned worker pool** in `lcpc-brakedown-pc`, `lcpc-2d`, and `lcpc-bench`.

---

## 1. Background & Multithreading Architecture

In upstream LCPC ([Wah21], commit `f0e2116`), multithreading parallelized across rows using Rayon (`par_chunks_mut(n_cols)`). In that row-level parallelism model, each thread independently traversed the recursive expander graph structures, performed sparse matrix-vector multiplications, and eagerly reduced modulo the field prime on every edge.

As analyzed in the SIMD-Spielman paper ([KDHR26] §3.5.2, Table 1), independent row-level or encoding-level parallelism across cores that share a Last-Level Cache (LLC / L3) suffers severe cache contention: two independent encodings sharing an LLC achieved only 1.38–1.41× the single-thread throughput ($N = 2^{22}, 2^{24}$) because each task competed for the shared cache budget.

### Expander-Level Collaborative Parallelism

In this work, our fork replaces row-level independent execution with **expander-level collaborative multithreading**:
1. **Collaborative Execution on Slices**: All $T$ worker threads operate collaboratively on the **exact same slice and the exact same expander graph level**.
2. **Right-Node Partitioning**: For an expander graph with $M_R = \text{graph.n\_right()}$ output columns, the right nodes are divided into $T$ contiguous, disjoint ranges:
   $$\text{Worker } t \in \{0, \dots, T-1\} \text{ computes } r \in [r_{\text{start}}(t), \; r_{\text{end}}(t))$$
   where partition sizes differ by at most 1 (via `split_range(M_R, t, T)`).
3. **Shared Read-Only Left Nodes in LLC**: All worker threads read concurrently from the **same left-node slice**, which remains hot in the shared LLC. Because left-node accesses are read-only, all CPU cores hit in shared cache with zero cache-coherency invalidations.
4. **Disjoint Writes & Private L1d Accumulators**: Each worker thread $t$ maintains its private scratch accumulator buffer of size $R_s \times \text{size\_of::<F::Acc>()}$ (resident in core $t$'s private L1d cache) and writes to a strictly disjoint range of destination columns.
5. **Zero Locks and Zero Atomics in Hot Kernels**: No locks, mutexes, or atomic instructions occur during inner-loop expander evaluation.

```text
Shared LLC:  [ Left Nodes of Current Slice: Columns 0 .. L-1 (Read-Only by all threads) ]
                   │                     │                     │
      ┌────────────┴────────────┐        │        ┌────────────┴────────────┐
      ▼                         ▼        ▼        ▼                         ▼
Worker 0 (core c[0])     Worker 1 (core c[1])    ...  Worker 7 (core c[7])
Private L1d Scratch      Private L1d Scratch         Private L1d Scratch
Computes Right Cols:     Computes Right Cols:        Computes Right Cols:
[ 0 .. M_R/8 )           [ M_R/8 .. 2M_R/8 )         [ 7M_R/8 .. M_R )
      │                         │                           │
      ▼                         ▼                           ▼
[ Dest Cols 0 .. M_R/8 ) [ Dest Cols M_R/8 .. ) ...   [ Dest Cols 7M_R/8 .. M_R )
```

---

## 2. Reusable Sense-Reversing Generation Barrier

Threads synchronize between expander graph levels (and between matrix transpose phases) using [`SenseReversingBarrier`].

### Mathematical Invariants & Memory Ordering

Between graph levels, output columns written by worker threads in level $K$ become input columns read by worker threads in level $K+1$. All writes from level $K$ must be guaranteed visible to all threads in level $K+1$.

The barrier enforces this using a release-acquire synchronization protocol on an atomic counter and generation counter:

1. **Arrival Phase (`count.fetch_sub(1, Ordering::AcqRel)`)**:
   - Each arriving thread decrements `count` with `Ordering::AcqRel`.
   - `Release` ensures that all memory writes performed by this thread prior to calling `barrier.wait()` are published and will be acquired by the last arriving thread.
   - `Acquire` ensures any memory operations released by earlier operations are acquired.

2. **Turnaround Phase (`count.store(N, Ordering::Release)` and `generation.fetch_add(1, Ordering::Release)`)**:
   - The last arriving thread observes `fetch_sub` return 1.
   - By transitivity of the release-acquire sequence on `count`, the last thread has acquired all preceding memory writes from all other $N - 1$ worker threads.
   - The last thread resets `count` to $N$ with `Ordering::Release`.
   - The last thread increments `generation` with `Ordering::Release`.

3. **Departure Phase (`generation.load(Ordering::Acquire)`)**:
   - Waiting threads spin/yield checking `generation.load(Ordering::Acquire)`.
   - When they observe the incremented generation, the `Acquire` load synchronizes-with the `Release` write to `generation` by the last thread.
   - By transitivity of the Release-Acquire chain:
     $$\text{All writes before wait}() \xrightarrow{\text{happens-before}} \text{All reads after wait}()$$
   - This provides total memory visibility across all $N$ threads with zero data races.

4. **Bounded Spin-Yield Strategy**:
   - In the wait loop, threads execute `std::hint::spin_loop()` for up to 64 iterations before yielding CPU time slices via `std::thread::yield_now()`.
   - This prevents priority inversion, lockup, and starvation when multiple worker threads share a physical core (such as $T = 8$ workers on the 7 available cores of the local workstation).

---

## 3. Persistent Pinned Worker Pool (`ThreadPool`)

Worker threads are managed by [`ThreadPool`]:
1. **Persistent Worker Lifecycles**: Worker threads are spawned **once** during `ThreadPool::new(T, cores)` via `std::thread::Builder::new().name(...).spawn(...)` and persist across all slices, recursion levels, and benchmark repetitions.
2. **Graceful Idle Parking**: When no encoding tasks are active (e.g. between benchmark runs, during serial setup, or during tests), worker threads park via condition variables (`Condvar`), consuming 0% CPU.
3. **Core Pinning**:
   - On Linux, worker $i$ pins itself to `cores[i % cores.len()]` via `libc::sched_setaffinity`.
   - On non-Linux or restricted container environments, pinning gracefully degrades with a logged warning.
   - Cores are supplied explicitly (`--cores`) or derived from the process affinity mask via `libc::sched_getaffinity`. CPU 2 is never implicitly picked.
4. **Clean Drop**: When dropped, the pool sets an atomic shutdown flag, wakes all workers via condition variable, and joins all thread handles cleanly.

---

## 4. Integration into PCS and Benchmarks

- **PCS Layer (`lcpc-2d`, `lcpc-brakedown-pc`)**:
  - `SdigEncodingS` stores configured thread count (`threads`) and an optional persistent `ThreadPool`.
  - Builders: `with_threads(T)`, `with_threads_and_cores(T, cores)`, `with_thread_pool(pool)`.
  - `commit(&coeffs, &enc)` delegates encoding to `enc.encode_rows`, which runs collaboratively across the pinned worker pool when $T > 1$.
  - Post-encoding stages (column hashing, Merkle tree construction via `hash_columns` and `merkleize`) continue using Rayon with zero regression.
- **Benchmark Binaries (`lcpc-bench`, `bench_encode`, `bench_pcs`)**:
  - Accept `--threads <N>` and `--cores <LIST>` flags and configure the encoding accordingly.
  - Automatically report thread count, explicit cores, and slice parameters in standard benchmark JSON output.

---

## 5. Verification & Test Suite

The test suite in `lcpc-brakedown-pc/src/tests.rs` validates all threading invariants:

| Test Name | Verified Property |
| :--- | :--- |
| `test_sense_reversing_barrier_ordering` | Multi-generation release-acquire barrier synchronization across 8 threads and 500 generations. |
| `test_pool_lifecycle` | Pool spawning, multi-job execution persistence, worker reuse, and clean drop for $T \in \{1, 2, 4, 7, 8\}$. |
| `test_pinning_respects_core_list` | Core list parsing (`0,1,3-5`), affinity query, and worker `sched_getaffinity` verification. |
| `test_thread_invariance_all_fields_and_modes` | **100% bit-exact equivalence** for $T \in \{1, 2, 4, 7, 8\} \times s \in \{1, 4\} \times \{\text{Mont127}, \text{Mers127}, \text{Fp2M61}\} \times \{\text{mode (a)}, \text{mode (b)}\}$ against $T=1, s=1$ reference. |
| `test_threading_flakiness_50x_loop` | 50 back-to-back iterations on $T=4$ and $T=8$ to detect race conditions, barrier misalignments, or deadlocks. |
| `test_pcs_commit_with_threads` | Full end-to-end `commit`, `prove`, `verify` across $T \in \{1, 2, 4, 7, 8\}$ verifying bit-exact Merkle roots and proof verification. |
