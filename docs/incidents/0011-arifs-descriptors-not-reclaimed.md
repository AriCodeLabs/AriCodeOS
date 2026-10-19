# Incident 0011: arifs descriptors are never reclaimed when a client dies

- Opened: 2026-10-18. Status: fixed 2026-10-19 (`fix/arifs-descriptor-reclaim`,
  ADR 0036), reproduced in QEMU before the fix.
- Found by: the independent audit of `fix/installer-state-read-error`
  (incident 0008), while reviewing what a full descriptor table means for
  the installer.
- Code: `services/arifs/src/main.rs`, `Fds` (the 16-slot table of open
  descriptors keyed by file index and owner endpoint) and the `Op::Open` /
  `Op::Create` arms that fill it.

## Observation

`Fds` has sixteen slots. A slot is freed only by `Op::Close` from the same
owner endpoint. Nothing frees the slots of a client that dies with files
open: the table keeps `(index, owner)` until arifs itself restarts.

Every run of the installer uses the main file space, so every installer
run has the same owner endpoint. An installer that is killed or crashes
between `open` and `close` (a timeout in the shell, a fault in strict
mode, a bug) leaves its descriptors in the table for good; after enough
such runs the table is full and every `open` answers `NoSpace`.

## Consequence for the installer

Before incident 0008 the installer found files through the directory
listing and read a failed request as "absent": a full table would have
made `updater/state` look missing and the next write would have replaced
it with one section. Since `fix/installer-state-read-error` the installer
opens the file and treats `NoSpace` like any other non-not-found status:
it refuses the run with `inst-state-unreadable` (or `inst-installed-
unknown`, `inst-pkg-read`), writes nothing and the fault names the error.
The failure is now visible and harmless to the data, but the service stays
unusable for that client until arifs restarts.

## Likely hook

ADR 0030 (`docs/decisions/0030-ipc-peer-closed.md`) gives endpoints a
peer-gone notification (`PeerClosed`): arifs serves every bound endpoint
through `ipc_receive_any` and can learn that an endpoint's owner is gone.
Descriptors (and a chunked replace in progress, which already has a
timeout for a vanished owner) should be released on that notification.
Note that the main file space is shared by every holder of the `install`
capability and the shell, so "owner gone" must be decided per client
endpoint, not per directory, before descriptors can be released safely.

## Reproduction

`fd_leaker` (`user/testprogs/src/bin/fd_leaker.rs`, manifest `caps = fs`):
opens one file of its private directory until fsd answers `NoSpace`, prints
`fd_leaker: holding 16 descriptors, the next open was refused (NoSpace);
exiting without closing any` and exits. Before the fix, the shell's next
`cat hola.txt` got `NoSpace` as well and so did every later `open` until
reboot; with sixteen slots and one leaker the table was full in one run.

## Mechanism (confirmed)

Two facts made the leak unrecoverable, both in `services/arifs/src/main.rs`
at `743c85b`:

1. The owner of a descriptor was the *endpoint slot* a request arrived on,
   and several processes shared each slot: every `install` holder, shell
   and sshd had a duplicate of the main endpoint (slot 0); every launch of
   the same application got a handle to one per-name `Bind`/`Share`
   endpoint ("same name and kind share an endpoint"). fsd could not say
   whose a descriptor was.
2. fsd held a `WRITE` capability on every namespace endpoint it created
   (`endpoint_create` gives every right and it duplicated `WRITE` from it
   for each launch), so the kernel's peer-gone report of ADR 0030 could
   never fire for them: the send side closes only when its *last*
   capability goes, and fsd's own never went.

## Fix (ADR 0036)

- Every client endpoint is a *session* fsd creates with
  `endpoint_create_pair`, keeping only the receive side: `Bind`, `Share`
  and the new `Attach` (the whole namespace; `Op` 13) give one session per
  grant. The broker uses `Attach` for the `install` capability, init for
  the console and SSH session shells, so every process that can die
  unexpectedly has an owner of its own.
- fsd asks the kernel whether a session's client is alive with a receive
  of zero timeout (`PeerClosed` once the send side is gone and the queue
  empty; ADR 0030): once a second and whenever a table is full before
  refusing. A retired session releases its descriptors, its chunked
  replace and its slot; descriptors left open are a fault of the client,
  said on the console and recorded as `fs fds reclaimed xN` (code 0) through
  the netd meter, now `libs/faults` (`aricode-faults`). Refusals are
  recorded too: `fs fds full` and `fs sessions full` (code 4).
- The table is `aricode_arifs::descriptors::Descriptors` (`libs/arifs`),
  host-tested: owner isolation, full-table refusal, reclaim of one owner.
- `diag` (`services` section) shows `descriptors 3/16 open, 16 reclaimed
  from dead clients: main 1, root 2` through `Op::Descriptors` (14).
- No kernel or ABI change.

## Evidence

`filesystem.descriptors_reclaimed` (`build/scenarios/
filesystem.descriptors_reclaimed/serial.log`), first run with the fix:

```text
ari> run fd_leaker
AriInit: launch fd_leaker with manifest [fs] memory 64 KiB
fsd: private directory 'fd_leaker'
fsd: fault fs fds full x1
fd_leaker: holding 16 descriptors, the next open was refused (NoSpace); exiting without closing any
run: fd_leaker exited with 0
ari> cat hola.txt
fsd: client gone (dir 'fd_leaker'), 16 descriptor(s) left open: reclaimed
fsd: fault fs fds reclaimed x16
Hola desde AriFS
ari> update
AriInit: launch installer with manifest [system install] memory 64 KiB
updater: no update sources: update add URL
ari> diag
          descriptors 0/16 open, 16 reclaimed from dead clients
  [11.024 s] fault reported by process 7: fs fds full x1 (code 4)
  [11.285 s] fault reported by process 7: fs fds reclaimed x16 (code 0)
```

The `fs fds full x1` is the leaker's own seventeenth open, refused while it
was alive (correct); the shell's `cat` found the table full, swept, and was
served. The reclaim happened at the shell's request, about 180 ms after
the leaker exited, not at the periodic sweep.

## Tests

- Host: `libs/arifs` (`descriptors` module: owner isolation, full table,
  reclaim), `libs/faults` (`report_many`), `sdk/proto` (`Attach`,
  `Descriptors`, `OwnerDescriptors` packing).
- QEMU: `filesystem.descriptors_reclaimed` (new); `filesystem.*`, `apps.*`,
  `update.*`, `shell.*`, `diag.*`, `ssh.*`, `ipc.*`, `sandbox.*` green with
  the change (sessions per launch, `install` and shells through `Attach`).

## Open

- Descriptors opened on the main endpoint itself (init, sshd) are released
  only by `Close`. init never exits; sshd is supervised with a bounded
  restart budget and closes what it opens, so a crash mid-read costs at
  most one descriptor per restart. Not worth a lease, which could not tell
  a leaked descriptor from a live one (ADR 0036, options).
- netd has the same shape of problem with sockets (incident 0012, open
  item); the same mechanism (per-client sessions and a liveness probe)
  applies.
