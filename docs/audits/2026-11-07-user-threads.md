# Audit: threads in a process (2026-11-07)

Independent, read-only audit of `feature/user-threads` (6e21301, b40da78, b6e475c;
ADR 0082). It checked each of the eight things the ADR's Context said would break
with a second thread, and found all eight fixed.

No High finding. Sound as checked: the address space, handles and DMA domain go
only when the last thread is reaped, and a thread never runs on a freed one; the
process stays in the table while any thread of it exists; the copy recovery
matches one instruction in ring 0 and nothing else reaches user memory; the
shootdown's order (CR3 published before its load; tables changed, a full fence,
then the published values read) and its only caller's context; the futex loses
no wake, keeps no more entries than waiting threads, and every waiter removes its
own; stack slots and guard pages.

| Finding | Fix |
|---|---|
| M1: a thread inside a call when its process's end began could claim the console or the PS/2 stream again for the dead process, after `finish` let them go: input lost until reboot, or typed input handed to a successor | the end is seen from its first moment (`ending`), a call checks it on entry as well as on exit, and the console and PS/2 cleanup runs again once the last thread is reaped |
| M2: `Pool::run(&self)` let two threads, or a job calling `run`, overwrite the job under the pool's threads: a use after free in safe code | `run(&mut self)` |
| L1: a place still being made (thread id 0) was woken by a kill: the kernel's own main thread | such places are not live threads |
| L2: a receive whose handle copy failed (a sibling unmapped the buffer) left handles the program never learned | the handles are taken back, as `pci_claim` does |
| L3: threads joined but not reaped did not count against the limit | the limit counts the larger of threads kept and threads not reaped |
| L4: `Pool::run(0, f)` still ran `f(0)` | returns at once |
| L5: missing tests | `user.thread_kill` (a kill from outside while every thread waits for ever, none left waiting on a futex), `user.thread_overflow` (a thread's guard page), and four threads touching 32 new pages at once in `user.threads` |

Not tested by a scenario: a copy racing a sibling's unmap (its recovery is tested
on its own by `memory.usercopy`) and the M1 race itself, whose fix is structural:
the cleanup runs where no thread of the process can run any more.

`user.thread_unmap` was checked against a kernel with the shootdown disabled: it
fails ("the writer still writes after the unmap").
