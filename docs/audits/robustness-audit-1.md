# Robustness audit 1: branch `fix/robustness-incidents`

- **Date:** 2026-10-17
- **Scope:** peer-gone notification on IPC (ADR 0030), sshd behaviour while
  the file service fails (incident 0001 follow-up), SMP self-test deadline
  (incident 0002).
- **Reviews:** two independent ones (the branch's own late reviewer and
  the coordinator's auditor). They agree the kernel core is correct and
  the branch was not mergeable as first submitted.

## Findings summary

Kernel core verified: `Cap` only via `Cap::new`, no `Clone`, moves keep
rights, reduced duplicates count kept sides, idempotent close/revoke,
iterative queue discard; `Cap::drop` leaf-safe; no lock inversion; no lost
wake-ups.

| Id | Severity | Finding |
|----|----------|---------|
| B1 | blocking | Busy-spin after a service is given up (console-bridge, compositor, sshd; servers continuing on receive errors). |
| B2 | blocking | History does not bisect; ADR number collision. |
| B3 | blocking | QEMU suite not run. |
| M1 | major | sshd lockout after `authorized_keys` deleted then re-added. |
| M2 | major | `process_wait` returns before capabilities are released. |
| M3 | major | Drops in `reap()` with interrupts off. |
| M4 | major (pre-existing) | Physical memory use-after-free through `map_foreign`. |
| L1 | low | Global swap per syscall. |
| L2 | low | Reference cycles leak. |
| L3 | low | Repeated handles lose a capability. |
| L4 | low | `host_key` overwritten on read error. |
| L5 | low | `Incomplete` counted as answered. |
| L6 | low | init probes not using the pair. |
| L7 | low | ADR item 7 inaccurate. |
| L8 | low | SMP deadline may hide regressions. |
| R1 | major | (reviewer 1) init may close endpoints of a still-alive stuck service. |

## Resolution

Recorded per finding in the commits that follow this file on the branch;
see the section "Resolution of audit 1" in ADR 0030 and the incident
files. B3 is run centrally by the coordinator (QEMU ports in use).
M4 is pre-existing and tracked as incident 0004, not fixed in this branch.

## Re-audit (2026-10-17)

All audit-1 findings resolved and every commit builds alone. One new
regression blocked the merge:

- **N1 (blocking).** `rpc::call_handles` returned `PeerClosed` both when
  the service endpoint had no receiver (send side, permanent) and when the
  server died holding this one request (reply side, not permanent: init
  keeps `READ` and relaunches the server on the same endpoint).
  `Error::endpoint_gone()` could not tell them apart, so console-bridge
  (always has a read pending) dropped a device forever after a driver crash
  (with several devices the keyboard stayed dead until reboot: the laptop
  `usb_input` fault case), the compositor lost GUI input for good, and sshd
  exited on a network driver crash during accept.
- Optional: console-bridge should park instead of exiting when every input
  is retired (avoids five noisy relaunches); the pair-packing comment's
  reasoning should be fixed. Latent: fsd's `ipc_receive_any` exits on
  `Revoked` from a granted namespace endpoint.

### Resolution

- `rpc::call_handles` maps a send refused with `PeerClosed`/`Revoked` to
  the new SDK-level `Error::ServiceGone` (permanent) and a reply side that
  closed to `Error::ReplyLost` (retry); no syscall returns either code.
  `InputError::service_gone()`/`reply_lost()`; `FsError::service_unavailable`
  updated; `NetError::Ipc` and the block client carry them as is.
- console-bridge and the compositor drop a device only on `ServiceGone` and
  read again after 50 ms on `ReplyLost`; console-bridge parks when every
  device is gone. sshd exits only on `ServiceGone` and listens again after
  `ReplyLost` or `BadSock` from a relaunched network driver.
- ADR 0030 corrected: only the send side is permanent; the fsd latent case
  is recorded. The pair-packing comment states the exact condition.
- Scenario `services.input_restart` (`input_restart`, programs
  `restart_input` and `restart_input_client`): an input driver crashes with
  a read pending; the client gets `ReplyLost`, init relaunches the driver
  and the device delivers events again. Not run here (QEMU is run
  centrally). `peer_client` case 1 now expects `ReplyLost` then
  `ServiceGone`.

### Re-audit verdict and last items (2026-10-17)

Verdict: safe to merge once the QEMU suite passes, after two small items:

1. sshd kept both `READ` and `WRITE` on the session output endpoint, so a
   crashed session shell was never seen as closed and the SSH session hung
   until the client disconnected. **Resolved:** `open_session` creates the
   input, output and reply endpoints with `endpoint_create_pair`; sshd keeps
   only the send side of the input and the receive side of the output.
   `PeerClosed` on the output without the end-of-stream message ends the
   session with exit status 139 (128 + SIGSEGV) and a log line. New
   scenario `ssh.shell_fault` (`shell_fault` lets the shell accept `fault`,
   a deliberate crash, tests only per ADR 0007): the crashed session exits
   with 139 and the next command is served.
2. `ipc_send` also returns `Revoked` when an extra handle is revoked, and
   `rpc` mapped it to `ServiceGone`. **Resolved:** `rpc::call_handles` maps
   `Revoked` to `ServiceGone` only when `object_kind` on the service
   endpoint itself reports `Revoked`; noted in ADR 0030.
