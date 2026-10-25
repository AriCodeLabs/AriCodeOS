# Incident 0028: fsd served a request after its own sweep retired the session

- Opened: 2026-10-25. Status: fixed on `fix/fsd-authority`.
- Found by: the failure-pattern audit of 2026-10-25 (audit C, H1; the
  OpenSSH and PHP2Shell pattern: state left behind by one path and
  trusted by the next). Not seen on a machine.
- Environment: any program launched through the broker with `caps = fs`
  (a private directory) or `read_files` (a read-only file).

## Evidence

`filesystem.retired_session` with the code of `develop` at `0fd600a`:
`fs_escape`, confined to `fs_escape/`, opened a file of the main
namespace.

```text
ari> run fs_escape
AriInit: launch fs_escape with manifest [fs] memory 64 KiB
fsd: private directory 'fs_escape'
fsd: client gone (dir 'fs_escape'), 16 descriptor(s) left open: reclaimed
fs_escape: opened hola.txt outside its directory (fd 0)
run: fs_escape exited with 10
```

## Mechanism

fsd keys every request by the endpoint it arrived on (`owner`, 0 = the
main endpoint) and looks the session up per request. When the 16-slot
descriptor table is full, an `Open` or `Create` first sweeps for dead
clients (incident 0011, ADR 0036): each session whose send side is closed
and whose queue is empty is retired and its descriptors reclaimed.

A client can close its session's send side after sending a request. The
sweep run while serving that very request then retires the requester's
session, and `handle` went on with `session(owner) == None`. Nothing
distinguished "no session" from the main endpoint: the prefix became
`""` (the whole namespace) and the read-only filter was skipped. The
request was served with init's authority, and the descriptor it opened
stayed on the freed slot for whoever got it next. The `Replace` path
had the same gap: probing the staging owner could retire the requester
and still record a transaction for it.

The same shape as this week's disclosures: authority derived from a
lookup whose "not found" fell back to the most privileged default.

## Fix

- `handle` refuses (`Denied`) any request from a client endpoint whose
  session is gone, before looking at the operation; the `Replace` path
  checks again after its probe. Nothing can be opened or staged for an
  owner that no longer exists, so no descriptor can outlive its session.
- Regression scenario `filesystem.retired_session` (test program
  `fs_escape`): it failed before the fix, as above.

## Follow-ups

The same audit found the related authority gaps in the file service
(sshd on the main endpoint, `read_files` without a policy, reserved
names); they are fixed on the same branch (ADR 0050).
