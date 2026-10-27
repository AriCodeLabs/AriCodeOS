# Incident 0049: The unit test level failed once and its output was lost

- Opened: 2026-10-27. Status: a failure of the same level with its log
  kept named the test (section "Recurrence"); fixed on
  `fix/ports-test-race`. Whether it was also the first failure cannot be
  shown.
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

The process (and, after the recurrence, the test): test output is never discarded. Every run
keeps its log (redirected to a file, or `tee`), so a red result can be
read instead of rerun (failure protocol). If the unit level fails again,
its log names the test, and this incident is reopened with it.

## Recurrence (2026-10-27)

The unit level failed again on `fix/updater-state` (after merging
develop), with its log kept (`/home/serverbig/aricode-updater-unit.log`):

```
test ports::tests::leases_are_distinct_free_and_released ... FAILED
leased port is bindable: Os { code: 98, kind: AddrInUse, message: "Address already in use" }
```

The test is xtask's, older than every branch of this batch (ADR 0040). A
`PortLease` picks free ports by binding port 0 and closes them again on
purpose (the program the port is for binds it later; the module says a
program that loses the race fails with its own error). The test then
bound each leased port itself and required success. But other tests of
the same binary run in parallel threads and bind port 0 and connect
clients, which take ports from the same ephemeral range, and
`listeners_never_take_a_leased_port` keeps rejected listeners open while
it searches, precisely on ports another lease holds; any other process on
the host draws from that range too. Any of them can take a just-freed
leased port in between: the assertion tested an exclusivity the design
does not promise.

## Fix

The test keeps its point (a lease does not hold its ports open) without
the race: if a port of the lease cannot be bound, fresh leases are tried,
up to five in all. A lease that held its ports open fails every attempt;
a chance collision almost never happens twice in a row. Whether the first
failure of this incident was this test is likely but cannot be shown,
since its output was discarded.
