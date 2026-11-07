# ADR 0082: Threads in a process

- **Date:** 2026-11-07
- **Status:** Accepted as stage F's first step of ADR 0080, on the owner's
  "continue" after bench3d 6. Implementation on `feature/user-threads`; the kernel
  part gets an independent audit before it merges.

## Context

A user process has one thread. The renderer that bench3d measures uses one of the
laptop's four (ADR 0080, 4c), and ADR 0080 lists threads as what simulation,
control and tensors need too. The kernel already has threads of its own: the
scheduler, kernel stacks and the per-thread FPU state of ADR 0065 work for any
number. What assumes one thread is the process around them.

A read-only map of the kernel (2026-11-07) found what would break with a second
thread on day one:

1. **Reaping:** the end of any user thread frees its process's address space,
   handles and DMA domain (`scheduler::reap` calls `release_address_space`), so
   the first thread to end pulls the page tables from under the others.
2. **A second end panics:** `finish` expects the process in the table, which
   `wait` removes once it was reaped.
3. **Kill wakes one thread:** `request_kill` wakes `main_tid` only; a thread in
   an endless wait would keep the process alive.
4. **First-touch pages:** two threads touching the same zero-fill page, the
   second finds it present and is killed instead of retrying.
5. **Copies against unmaps:** `copy_from_user` and `copy_to_user` check a range
   under the process lock and copy after it; another thread's `mmio_unmap` in
   between makes the kernel fault in ring 0, which panics.
6. **TLB:** an unmap of user memory invalidates only the CPU that made it
   (`flush_own_user_range` says so); another CPU running the same address space
   keeps a stale entry into freed frames.
7. **Check then act:** `pci_claim` and `ipc_send` check room or rights in one hold
   of the lock and act in another ("this process is single-threaded").
8. **The user runtime:** its heap lock panics after 2^26 spins, which a thread
   spinning while the holder is preempted can reach.

The same map found no fault in today's one-thread world: the kernel heap unmaps
only pages it has just mapped and never handed out, and kernel stacks never reuse
their addresses.

## What others do, and where they failed

- **Linux** `clone` with flags (`CLONE_VM | CLONE_THREAD | ...`): one call, many
  combinations, years of corner cases. `futex` grew to over a dozen operations;
  its priority-inheritance and requeue paths gave CVE-2014-3153 (towelroot),
  kernel code execution from any process.
- **Zircon** (Fuchsia): `zx_thread_create` then `zx_thread_start(entry, stack,
  arg)`; the program supplies the stack. `zx_futex_wait/wake`, with an owner for
  priority inheritance. A process lives while any thread does, until
  `zx_process_exit` ends them all.
- **seL4**: a thread is a TCB object made from untyped memory; user space owns
  stacks and IPC buffers. No futex: notification objects.
- **Redox**: thread creation moved from a `clone` syscall into user space over the
  `proc:` scheme; futex for waits.
- **Hubris**: no threads in a task, on purpose: tasks are the unit.

Where they agree: the stack is user memory, a futex is a word in the process's
memory with wait and wake, and a whole process can be ended at once. Where they
hurt: too many operations on one call, futex features beyond wait and wake, and
exit rules that leave threads behind.

## Decision

1. **Three thread calls and two futex calls** (ABI 9):
   - `thread_create(entry, arg) -> tid`: a thread of the calling process starts
     at `entry(arg)` on a new stack the kernel gives it.
   - `thread_exit(value)`: ends the calling thread. From the main thread it ends
     the process, as `process_exit`.
   - `thread_join(tid, timeout_ms) -> value`: waits for a thread of the same
     process to end and forgets it.
   - `futex_wait(addr, expected, timeout_ms)`: sleeps while the aligned `u32` at
     `addr` holds `expected`, until woken or the timeout.
   - `futex_wake(addr, count) -> woken`.
   Futexes are private to the process, keyed by its id and the address. No
   priority inheritance, no requeue, no shared futexes between processes.
2. **Stacks from the kernel**, 64 KiB each as the main one, with a guard page
   below, in a region of their own below the MMIO region, backed on first touch.
   The kernel picks them so a program cannot point a thread at a stack it does
   not own; the guard turns an overflow into a kill that names it.
3. **A limit per process**, a spawn parameter (`spawn_limits`, 0 for the default
   of 8 threads with the main one); each thread costs the kernel a 32 KiB kernel
   stack and an FPU area, which the limit bounds.
4. **One end for the process.** It ends when its main thread ends, when any thread
   faults, or when it is killed; every other thread is then ended too: each one
   stops at its next entry to the kernel or tick, and every wait it is in wakes.
   `finish` runs once, for the first end. The address space, handles and DMA
   domain go when the last thread has been reaped.
5. **TLB shootdown for user memory.** Each CPU records the address space it runs.
   An unmap of user memory invalidates the local CPU and every CPU running the
   same address space, and only then frees the frames or the range.
6. **Copies that cannot fault the kernel.** User copies go through one routine
   whose page faults return an error instead of a panic (an exception table, as
   Linux and Zircon have), and first-touch pages are backed on the way.
7. **Check and act in one hold** for `pci_claim` and `ipc_send`; a first-touch
   page found present means "retry".
8. **The runtime:** `aricode_rt::thread::spawn` and `join`; a `Mutex` and a
   `Condvar`-like wait on futexes; the heap lock spins, then yields, never panics
   from contention alone. `input::this_instance()` keeps meaning the main thread.

## Order of work

1. Exception table for user copies; check and act in one hold; first-touch retry.
2. Per-CPU address space and targeted shootdown for user unmaps.
3. Process lifecycle with a thread set: one end, kill for all, reap on the last.
4. The calls, stacks, futex, limit; ABI 9.
5. Runtime and SDK.
6. Tests: threads that compute and join, a futex ping-pong, kill with threads in
   endless waits, an unmap while another thread copies and while another CPU
   touches the range, faults in a non-main thread, the limit, overflow into the
   guard; the existing reap and FPU scenarios with threads.
7. Independent audit of 1 to 4.
8. Then render3d in bands across threads, and bench3d's figures with 1 and 4.

## Consequences

- A process may use every CPU; its kernel memory per thread is bounded by the
  limit, not by its DMA quota.
- A killed thread spinning in ring 3 on another CPU stops at its next tick, up to
  10 ms later; an IPI to stop it at once can come with the scheduler work of
  ADR 0080 if measurement asks for it.
- Programs written for one thread keep working: nothing changes until they call
  `thread_create`.
