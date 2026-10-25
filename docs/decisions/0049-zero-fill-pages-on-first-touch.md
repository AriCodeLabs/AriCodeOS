# ADR 0049: Zero-fill pages backed on first touch, and what happens when memory runs out

- **Date:** 2026-10-24
- **Status:** Accepted (performance audit 731c398, finding D)

## Context

The loader allocated and zeroed every page of every ELF segment when a
process started. Every program linked with the runtime carries a static
256 KiB heap arena in its `.bss`, so each held that much physical memory
from its first instruction, used or not; with the user stack, a small
driver cost over 300 KiB. The audit asked to measure first and then to
choose per-program budgets or on-demand backing "with an explicit OOM
policy". Measured after the change, in a QEMU boot: init backs 24 KiB of
its 260 KiB of zero-fill memory, `virtio_net` 60, the shell 12, sshd 0.

## Decision

1. **What is backed on demand.** The whole pages of a writable segment
   past its file contents (`Segment::zero_fill_pages`): `.bss` and the
   heap arena. The page that holds the end of the file contents, every
   read-only or executable page and the user stack stay backed at load
   (the stack keeps its unmapped guard page meaning "overflow").

2. **First touch.** A ring-3 page fault on a reserved page that is not
   present gets a zeroed frame mapped with the segment's flags, and the
   instruction runs again. An instruction fetch from a reserved
   no-execute page is an ordinary fault (no frame is spent on it). The
   kernel never touches a reserved page unbacked: every access to user
   memory goes through `check_user_range`, which backs the pages first
   (one per hold of the global process lock).

3. **When memory runs out.** First-touch backing never takes the last
   `KERNEL_RESERVE_FRAMES` = 256 frames (1 MiB): the kernel's own heap,
   page tables and the kill path itself must still find memory, since a
   kernel allocation failure is a panic. Past that:
   - a process whose own access needs a page is killed, with the reason
     `out of memory` in the event ring and `USER OUT OF MEMORY` on the
     serial console; a supervised service is restarted under init's
     backoff and budget like any other death;
   - a system call that would write into an unbacked page fails with
     `Limit` and takes nothing: calls that consume something (a message,
     its capabilities, console input) check and back their whole output
     buffers before they consume it.
   The total a process can reach is what it could reach before (its
   segments); only the moment its memory is taken moves, from its start
   to its first use. Spawns no longer fail for memory a program would
   never touch. Since 2026-10-25 (audit C, L1) a spawn's own segment and
   stack frames respect the reserve too, and a spawn that fails gives
   back every frame it took.

4. **Visible.** `sysinfo::ON_DEMAND` gives, per live process, the
   zero-fill memory reserved and backed so far; `diag` prints the table
   under the user stacks.

## Consequences

- Each runtime program costs the pages it uses instead of its whole
  arena; the per-program arena size question the audit raised is moot
  while 256 KiB of address space stays reserved.
- Memory is now overcommitted: the sum of the reserved pages of all
  processes can exceed what is free. Exhaustion shows up as a process
  killed for `out of memory`, not at spawn. The kernel's reserve keeps it
  from taking the kernel down.
- `ON_DEMAND` lets a holder of `READ` on `SYSTEM` watch another process's
  heap grow, page granular: the same kind of weak side channel as
  `STACKS` (ADR 0039), under the same right.

## Tests

- Host: `Segment::zero_fill_pages` for every segment shape.
- `memory.on_demand` (full): `lazy_probe` has the kernel write into an
  untouched static page (a system call's output), uses 128 KiB of heap,
  and finds its own entry with less backed than reserved; `diag` prints
  the table.
- The full suite with 1 and 4 CPUs: every process starts this way.
