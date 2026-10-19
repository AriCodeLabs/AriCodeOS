# ADR 0036: File descriptors follow their client

- **Date:** 2026-10-19
- **Status:** Accepted (incident 0011; amends ADR 0016 and ADR 0018, uses
  ADR 0030)

## Context

fsd keeps 16 open descriptors for every client together, each keyed by
(file index, owner endpoint) so that a descriptor only answers the endpoint
that opened it (audit `e9fdf38`). A slot was freed only by an explicit
`Close` from the same owner. Incident 0011: a client that exits without
closing (a killed installer, a crashed application, a shell command that
faulted) leaves its descriptors in the table until fsd restarts, and once
sixteen have piled up every `Open` answers `NoSpace`. Since incident 0008
the installer refuses such a run visibly (`inst-state-unreadable`,
`inst-installed-unknown`) instead of reading a failed open as absence, so
the data is safe, but installs and `update` stay blocked until a reboot.

fsd could not clean up because it did not know *whose* the descriptors
were. Every holder of the `install` capability, every shell and sshd got a
duplicate of the main endpoint: one owner, many processes. Private
directories (`Bind`) and shared files (`Share`, ADR 0016) were one endpoint
per *name*, shared by every launch of the same application, and fsd held a
`WRITE` capability on each of them itself (`endpoint_create` gives every
right), so the kernel could never report that endpoint's send side as
closed (ADR 0030 closes a side when its last capability goes).

## Options considered

(a) **Per-client endpoints.** Whoever launches a process (init, the broker)
asks fsd for a fresh endpoint for it and fsd keys the descriptors by that
endpoint; when the kernel reports the peer gone, fsd frees them all. Exact:
the kernel knows when a process is reaped (ADR 0030, item 4), and no
guessing about idle time is involved.

(b) **Descriptor leases.** A descriptor unused for N seconds is reclaimed
with a fault event. Covers every client, including those that share the
main endpoint, but it cannot tell a leaked descriptor from a live one: a
shell holding a file open, or init reading a 256 KiB package in 4 KiB steps
from a slow stick behind a 120 s reply wait, would lose a descriptor that
is in use. A timeout chosen to be safe for them is too long to help, and a
reclaimed live descriptor is a silent data error of exactly the kind the
failure protocol forbids.

**Decision: (a) for every process init or the broker launches, and no
lease.** Every client that can die unexpectedly now has its own session;
what remains on the main endpoint is init itself (which never exits) and
sshd (supervised, bounded restart budget: at most a handful of descriptors
across a boot, and it closes what it opens). The main endpoint's
descriptors are not reclaimed by time; the residual is documented in the
incident and shown in `diag`.

## Decision

1. **Sessions.** Every endpoint fsd serves besides its main one is a
   *session* created on request of the main endpoint, of three kinds: a
   private directory (`Bind`), a read-only file (`Share`) and, new, the
   whole namespace (`Attach`, `Op` 13: what the main endpoint sees except
   `Bind`, `Share` and `Attach` themselves). fsd creates each with
   `endpoint_create_pair` (ADR 0030, item 5), keeps **only the receive
   side** and transfers the send side in the reply. One session per grant:
   two launches of the same application get two sessions, never one.
2. **Owner = session.** A descriptor belongs to the session (or the main
   endpoint) it was opened on, as before; the chunked replace in progress
   (ADR 0018) likewise. The table is `aricode_arifs::descriptors`
   (`libs/arifs`, host-tested): open, get and close by owner, `reclaim`
   of one owner, counts per owner.
3. **The kernel says when a client is gone.** Nobody but the client (and
   whoever it transferred its handle to) holds a session's send side, so
   when the process is reaped its send side closes (ADR 0030, items 2 and
   4). fsd learns it without waiting: a receive with a zero timeout on a
   session endpoint returns `PeerClosed` once the send side is closed and
   the queue is empty, `Timeout` while the client lives. fsd asks this way
   (a *sweep*) once a second from its loop and, on demand, whenever a table
   is full: an `Open`/`Create` that finds no free descriptor, or a grant
   that finds no free session slot, sweeps first and refuses only if the
   table is still full; a `Replace` that finds the one-slot journal held by
   another session probes that session alone before answering `Busy`. A request a sweep happens to take from a live
   session is kept and served before the next wait, so the probe never
   loses a message. No kernel or ABI change was needed: `ipc_receive_any`
   keeps reporting `PeerClosed` only when every endpoint is closed (ADR
   0030, item 2), and the probe uses `ipc_receive` as specified there.
4. **Retiring a session** releases its descriptors, abandons its replace
   in progress (the journal is no longer held until fsd restarts, closing
   the ADR 0018 consequence), closes fsd's handle and frees the slot for the
   next grant. A session retired with descriptors still open is a fault of
   its client: fsd says `fsd: client gone (dir 'fd_leaker'), 16
   descriptor(s) left open: reclaimed` and records `fs fds reclaimed xN`
   (N = descriptors) in the kernel event ring, code 0 (nobody was
   refused). A client that closed everything retires silently.
5. **Refusals are recorded** like netd's (incident 0012, ADR 0035): `fs
   fds full` (code 4, `NoSpace`) when an open is refused after a sweep,
   `fs sessions full` (code 4) when a grant is. The meter is the one netd
   uses, moved to its own crate `aricode-faults` (`libs/faults`, host
   tests) with a `report_many` so one dead client with sixteen descriptors
   is recorded as `x16` and not as `x1` now and `x15` a second later.
6. **Who gets a session.** The broker asks `Bind`/`Share` as before (now
   per launch) and `Attach` for the `install` capability instead of a
   duplicate of the main endpoint. init asks `Attach` for the console shell
   and for every SSH session shell (`spawn_shell`), so a shell that faults
   (`ssh.shell_fault`) or a `cat` interrupted by a crash leaves nothing
   behind; a refused attach is said on the console (`AriInit: file session
   refused by fsd (status N)`) and the launch fails rather than falling
   back to the shared endpoint. Up to `IPC_MAX_ANY - 1 = 15` sessions at
   once (they are served together with the main endpoint by one
   `ipc_receive_any`). That is not a guaranteed budget: a launch takes one
   `Bind` plus one `Share` per `read_files` entry (`MAX_READ_FILES = 4`),
   so the worst case is `MAX_RUNNING = 4` programs × 5 + the console shell
   + `MAX_SESSIONS = 4` SSH shells + an installer among the four = 26.
   The broker does not budget sessions per launch (it would have to know
   fsd's free count, which changes as clients die); a grant that finds the
   table full sweeps first and, if it is still full, the launch fails
   visibly (`Failed` to the shell, `fs sessions full` in `diag`). The
   programs shipped today ask for at most one `read_files` entry, so the
   table holds the four running programs and the five shells with room to
   spare; a wider `IPC_MAX_ANY` is an ABI change left for when a real
   workload needs it.
7. **`diag` shows the table** (`Op::Descriptors`, 14, main endpoint and
   `Attach` sessions only; `Denied` from a private directory or a shared
   file): `descriptors 3/16 open, 16 reclaimed from dead clients: main 1,
   root 2`, one entry per owner holding a descriptor, labelled `main`,
   `root`, `dir NAME` or `file NAME`. The counts travel packed in
   `Reply::value` (`fs::OwnerDescriptors`, host-tested).

## Privilege analysis

- A session grants no more than its holder had: an `Attach` session is the
  main endpoint minus `Bind`, `Share` and `Attach` (grants stay with init),
  so the installer and the shells *lose* the ability to create namespaces
  they never used. `Bind` and `Share` sessions keep ADR 0016's rules.
- No client gains access to another's descriptors: ownership is still the
  endpoint a request arrived on, and the owner number of a retired session
  is reused only after its descriptors were released (`retire` does both
  before the slot is free). Guessing a descriptor number still gets
  `BadFd`.
- Two launches of one application no longer share an endpoint, so one
  instance cannot read, write or close the other's files through shared
  descriptor numbers (possible before through the per-name endpoint).
- The shell's own descriptors live as long as the shell: a session is
  retired only when every holder of its send side is gone. A process that
  duplicates and transfers its session handle extends its lifetime to the
  receiver, which is the capability model working as intended.
- fsd still trusts the main endpoint for grants; only init holds it.
- `Op::Descriptors` tells an `Attach` holder which owners exist and how
  many descriptors each holds (names of private directories included).
  Those holders are the shells and the installer, which already list every
  file; private directories and shared files are refused.

## Rejected alternatives

- A new syscall or an `ipc_receive_any` variant reporting which endpoint
  closed: more ABI for what a zero-timeout receive already answers, and
  ADR 0030's `receive_any` contract would change for every server.
- Leases (option b above): cannot distinguish leaked from live.
- Keeping one endpoint per application name and counting launches in the
  broker: the broker would have to tell fsd when the last instance died,
  duplicating what the kernel already knows.

## Consequences

- `filesystem.descriptors_reclaimed`: `run fd_leaker` (manifest `caps =
  fs`) fills the table and exits without closing; the shell's next `cat`
  triggers the on-demand sweep and reads, `update` runs the installer
  through an `install` session and reaches the updater, and `diag` shows
  `descriptors 0/16 open, 16 reclaimed from dead clients`, `fs fds full x1`
  (the leaker's own 17th open) and `fs fds reclaimed x16`.
- The scenarios that assert `fsd: private directory 'NAME'` and `fsd:
  shared file 'NAME' (read-only)` still see those lines, now once per
  launch.
- `sandbox.manifest`: `fs_victim`'s half-done chunked replace is now
  released with its session, so `fs_probe`'s replace goes through at once
  (`journal released with its owner`, asserted in full) instead of after
  the 2 s staging timeout. That timeout stays for an owner that is alive
  but idle (and for the main endpoint, which is never retired), but no
  scenario exercises it any more; `Staging::abandoned` is two lines in fsd
  and a host test would need it factored out, which is not worth doing for
  a path only a stuck live client reaches.
- Protocol additions are additive (`Op` 13 and 14; `VERSION` stays 1); an
  old server answers them `Invalid`.
- Not covered: descriptors opened on the main endpoint itself (init, sshd)
  are released only by `Close`; sshd is supervised and closes what it
  opens. A session holder that transfers its handle to a long-lived process
  keeps its descriptors alive through it. The sweep costs one syscall per
  live session per second.
