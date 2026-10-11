# ADR 0015: Asynchronous broker launches and observable limits

- **Date:** 2026-09-28
- **Status:** Accepted

## Context

The broker (init) ran each `run` synchronously: `launch` waited for the
program to exit with no timeout. Meanwhile init neither watched the
resident services (ADR 0011) nor served other requests: a program that
never finished blocked supervision entirely. The 2026-09-28 study
(stage 3, section D) asks for counters and predictable rejection in
resource allocation, and for another application to keep making progress
under load.

## Decision

1. **Asynchronous launches.** The broker launches the program and stores
   `(process, reply endpoint)` in a slot; its loop checks the slots with
   `process_wait(0)` and replies (exit code or `Killed`) when the program
   finishes. `run` in the shell still waits for the reply, but init no
   longer stalls.
2. **Background launch** (`Request::DETACH`, `sys::launch::spawn`,
   `spawn PROG [ARG]` in the shell): the broker replies `Started` as soon
   as the process exists and closes its handle when it finishes; the shell
   does not wait.
3. **Limit on running programs** (`MAX_RUNNING = 4` slots): an extra
   launch gets `Status::Limit`, predictable and side-effect free. It is the
   broker's first quota; the kernel's quotas are still the handle table
   (256 per process, `Limit`), each endpoint's queue (16 messages, bounded
   wait) and the manifest's DMA memory.
4. **No priorities**: the round-robin scheduler shares the CPU among ready
   threads; with one CPU and four programs consuming it, the shell and a
   normal application keep making progress (`limits.hogs`). CPU budgets
   and priorities are left for a later stage with measurements.

## Consequences

- `run` no longer blocks supervision: a hung program does not prevent
  restarting a dead driver.
- `limits.hogs` (one CPU): three `cpu_hog` in the background; in the
  fourth slot `handle_hog` exhausts its table (252 endpoints) and releases
  it, and `hello_app` launches, writes to fsd and exits; the fourth hog
  takes the last slot and the fifth is rejected; `ps` shows the hogs.
- Programs run with `run` also consume slots: with all four taken by
  background programs nothing else can launch until one of them finishes.
- A background program cannot be stopped from the shell yet; it dies at
  shutdown. Stopping it requires a process capability transferable to the
  shell (later stage).
