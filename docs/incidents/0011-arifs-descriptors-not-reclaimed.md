# Incident 0011: arifs descriptors are never reclaimed when a client dies

- Opened: 2026-10-18. Status: open, found by reading (not reproduced).
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

## Next

Not fixed in `fix/installer-state-read-error` (out of its scope). Needs: a
reproduction (a test program that opens a file and exits without closing,
sixteen times, then any `open`), the release on `PeerClosed`, a fault
event when a client's descriptors are reclaimed, and a scenario.
