# Incident 0049: The unit test level failed once and its output was lost

- Opened: 2026-10-27. Status: not reproduced; open (watch item).
- Found by: the coordinator, applying the audit follow-ups of incident
  0046 on `fix/sshd-auth-accounting` (worktree at `ffc8b97` plus the
  uncommitted follow-up: a file digest in sshd's refused-lines state and
  documentation).
- Environment: the development host, while another worktree ran the full
  QEMU suite (`fix/kernel-infos`, ten guests at a time).

## Evidence

`cargo xtask test --level unit` exited with status 1 once. Its output had
been sent to `/dev/null`, so which test (or which build step) failed is
not known. `cargo xtask check` had passed just before on the same tree.

Seven later runs on the same tree passed (exit 0): two right after, then
five in a row with every log kept (`/home/serverbig/aricode-sshd-unit-*.log`
on the host), the first two of them while the full suite had ten QEMU
guests running, the other three on an idle host.

## Mechanism

Unknown. The host tests that depend on the host are the OpenSSH and
OpenSSL integration tests (`libs/ssh/tests/openssh.rs`,
`libs/tls/tests/openssl.rs`): they bind ports chosen by the kernel
(`127.0.0.1:0`), start real `ssh`/`openssl s_server` processes and wait
on 20 s socket timeouts and an 800 ms sleep, so a heavily loaded host is
the first suspect; nothing confirms it.

## What changed

The process, not the code: test output is never discarded. Every run
keeps its log (redirected to a file, or `tee`), so a red result can be
read instead of rerun (failure protocol). If the unit level fails again,
its log names the test, and this incident is reopened with it.
