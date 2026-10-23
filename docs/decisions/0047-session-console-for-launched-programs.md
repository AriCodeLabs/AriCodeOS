# ADR 0047: A session console for programs launched from an SSH shell

- **Date:** 2026-10-23
- **Status:** Accepted (extends ADR 0025, ADR 0015 and the launch broker
  of Phase 28; closes the console follow-up of ADR 0044)

## Context

A session shell (ADR 0025) writes to its SSH client through the send side
of an endpoint pair whose receive side sshd holds (`SESSION_OUT`, ADR
0030). The programs it runs do not: `run bench`, the updater and the
installer write with `console_write` on the console handle every process
receives at `0x0001_0003`, which is the kernel console, so their output
lands on the machine's screen and the remote user sees nothing but the
exit code. Two replays grew around that: the shell shows the updater's `last.log`
(a copy of everything it printed) after the updater exits, and the
install history after the installer. Every other program (`bench` first, on the reference laptop)
was simply silent over SSH, and during the dev channel hardware test an
empty SSH answer (incident 0025) could not be told apart from output sent
to the wrong place.

Constraints:

- Programs must not change. Installed packages are signed and already on
  machines; a fix that needs every program rebuilt against a new SDK would
  leave them silent.
- sshd reads an **empty** message on the session output as "the shell
  exited" and ends the session. A launched program must not be able to
  send one, nor transfer handles into the session, nor read from it.
- Nothing new is granted by default (owner's rule: no open doors). A
  program run from the console keeps writing to the kernel console.

## Decision

1. **Session console object.** The kernel gains a second console kind: a
   *session console* that wraps the capability to an endpoint's send side.
   `object_kind` reports it as `CONSOLE`, so a program sees a console.
   `console_write` on it validates the text exactly as on the kernel
   console (at most 4096 bytes, UTF-8) and sends it to the endpoint as one
   or more messages of at most `IPC_MAX_DATA` bytes, waiting up to 2 s per
   message for queue space (backpressure, as the shell's own writes). It
   returns the bytes sent: all of them, fewer when a later message could
   not be sent, or an error when none was (`PeerClosed` once the session
   is gone, `Revoked` if the endpoint was revoked, `Timeout` if the reader
   stalls); a zero-length write sends nothing. `console_read` and
   `console_inject` refuse it (`Denied`: it has neither `READ` nor
   `MANAGE`), and `ipc_send` refuses it (`Invalid`) because it is not an
   endpoint: the only operation is writing non-empty text. The wrapper
   holds a capability to the endpoint's send side only, made at spawn
   from the handle given, which keeps that side open (ADR 0030) while any
   handle to the session console lives.

2. **Spawn places it at the console slot.** `spawn_limits` gains a flag,
   `CONSOLE_FIRST` (bit 63). With it, the first transferred handle must be
   an endpoint with `WRITE` (and `TRANSFER`, like any transferred handle);
   it does not appear at `0x0001_0004` but becomes the child's console at
   `0x0001_0003`, with `WRITE | DUPLICATE` as the kernel console has, and
   the remaining handles start at `0x0001_0004` as before. The flag with no
   handles, or with a first handle that is not an endpoint, is `Invalid`;
   with an endpoint side it cannot write to (a receive side), `Denied`.
   Either way nothing is taken from the caller. Both `process_spawn` and
   `process_spawn_from` accept it. ABI version 7.

3. **The broker passes it only when asked.** The launch request (protocol
   6) gains the flag `CONSOLE`: the request then carries a second handle,
   after the reply endpoint, which the broker hands to the kernel with
   `CONSOLE_FIRST`. A request with the flag and no second handle, a second
   handle without the flag, or the flag on a detached launch is `Invalid`,
   and every handle received is closed. Detached launches keep the kernel
   console: a background program outlives the command and may outlive the
   session. The broker adds no capability the requester did not hold: the
   requester gives up a send right it already had.

4. **The session shell uses it for every waited launch.** In a session,
   `run`, the updater and the installer get a duplicate of `SESSION_OUT`
   with `WRITE | TRANSFER` as their console; `spawn` and the console shell
   are unchanged. The output arrives in order with the shell's own lines,
   because the shell waits for the program before printing again.

5. **The replays go.** The shell no longer prints the updater's
   `last.log` or the install history after those programs run: both now
   write to the session directly. The updater keeps writing `last.log`,
   now as the record of its last run for later reading (`cat
   updater/last.log`, or `cargo xtask fs-cat` on a stick dump, as in
   incident 0001), and the history stays in `updater/history.log`.

## Consequences

- `run bench`, `upgrade` and any program, installed packages included,
  show their output over SSH without being rebuilt.
- A program run from a session can no longer write to the physical
  console. That is intended: the remote user is the one who asked.
- While a waited program runs, it also holds the session's send side: if
  the shell is killed meanwhile, sshd sees the session end when the
  program exits rather than at once.
- A reader that stalls slows a writing program by up to 2 s per message
  before that message is lost; a closed session makes writes fail fast,
  and `sys::print` ignores the error as it always did. The backpressure
  is real because sshd stops taking session messages while it holds
  64 KiB the client's window has not accepted, and takes at most 16 per
  pass; typed input it sends without waiting, from a buffer of 64 KiB
  (audit, M1 and L4).
- A launched program's text reaches the remote terminal as written,
  control sequences included, as `cat` of a file always did: what a
  program prints is trusted as much as the program the owner ran.
- Kernel and IPC change: audited before merging (test levels skill). The
  audit (no high findings) also had the broker refuse a launch whose
  handles would overflow its list instead of starting the program
  without the last one (L3).

## Tests

- `init.boot` (smoke, with `selftest`): init spawns `console_probe` with
  the send side of a new pair as console and reads the probe's text on the
  receive side; a spawn with `CONSOLE_FIRST` and that receive side is
  `Denied`, with a non-endpoint handle or no handle `Invalid`, and the
  handles stay with init.
- `console_probe` checks, from inside, that its console reports `CONSOLE`,
  refuses `ipc_send`, refuses a duplicate with `TRANSFER`, and accepts a
  zero-length write that does not end the session.
- `ssh.exec` (full): `run console_probe` over SSH shows the probe's lines
  in the session and not on the machine's console; `update` keeps showing
  the updater's lines, now written live.
- Host tests: `spawn_limits` packing with the flag and the launch request
  round trip with `CONSOLE`.
