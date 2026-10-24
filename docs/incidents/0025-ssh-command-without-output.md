# Incident 0025: an SSH command that came back empty and never ran

- Opened: 2026-10-23. Status: open, not reproduced; observation only.
- Found by: the hardware test of the development channel (ADR 0045) on the
  reference laptop, over SSH from the build host.
- Environment: reference laptop (`rtl_net`, `sshd`), running stable
  system 12 (develop before ADR 0045); host OpenSSH client with
  `-o BatchMode=yes -o ConnectTimeout=5`; update server
  `cargo xtask serve-update --port 8765` on the host.

## What was seen

`ssh ari@192.168.1.27 upgrade`, sent a minute after
`cargo xtask publish-update --version 13` had finished, printed nothing at
all: no updater line, no usage line, no error from the client. The client's
exit code was not recorded (the command was in a chain without checks).
The update server's log shows no request from the laptop between the
previous `update` and the next one, so the updater never fetched the index.
Seconds-to-minutes later the same command on the same boot printed the
full upgrade and installed system 13 in 7 s.

The whole chain that contained it (three reboots included) took 142 s, so
the empty command returned quickly: it was not a hang cut by the client's
300 s `timeout`.

## What it is not

Every path of the shell's `upgrade` prints something before returning:
the usage line, `update: installer exited with N` or a launch failure,
the updater log (shown in an SSH session after the updater exits) or
`update: updater exited with N`. An empty answer without any request to
the server therefore means the command did not reach the shell, or its
output did not reach the client, rather than an updater failure.

## Reproduction attempts

36 back-to-back commands (`update sources`, `uptime`, `update`, twelve
rounds) on system 13 right after: every one exited 0 with output. Every
later step of the same session (reboots, `update`, `upgrade --channel dev`,
`update rollback`, about 30 commands) behaved normally.

## Open

- Record the SSH client's exit code on every laptop command (the session
  scripts now stop on a non-zero code, an empty answer or an
  `exited with` line), so a recurrence says whether the connection was
  refused, closed by `sshd` or completed with nothing written.
- If it recurs, read `diag` before anything else on the same boot: the
  kernel event list shows whether a `shell` process was created for the
  command, which separates "never reached the shell" from "output lost".
- Related: programs run from an SSH shell wrote on the physical console
  (ADR 0044 follow-up), so output going to the wrong place looked like
  this. ADR 0047 (2026-10-23) sends their output to the session; a
  recurrence after it is not that.
