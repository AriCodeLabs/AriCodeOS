# Incident 0036: TLS failed open without a clock or without random bytes

- Opened: 2026-10-25. Status: fixed on `fix/tls-time-and-seed`.
- Found by: the failure-pattern audit of 2026-10-25 (audit D, M1; audit
  C, L9). Not seen on a machine.
- Environment: every HTTPS client (`aricode-https`: the shell's `http`,
  the updater).

## Evidence

- Time (D-M1): `sdk/https` passed `sys::time::unix_seconds`, which is
  `None` when the kernel cannot read the real-time clock, to the TLS
  client; `verify_chain` skips the validity dates when given `None`. A
  machine without a clock, or whose clock reads before 2024 (a dead CMOS
  battery: the kernel rejects such years and reports no clock), accepted
  expired certificates.
- Seed (C-L9): the key exchange's seed came from `sys::random` with its
  result ignored: with a bad handle or too few bytes, the X25519 secret was
  all zeros, a key anyone can compute, with no error.

## Fix

- `aricode_x509::check_time` gives `ValidityTime::Clock` when the clock
  reads at or after `TIME_FLOOR` (the release day, raised at each release,
  docs/WORKFLOW.md): both dates checked. Otherwise `AtLeast(TIME_FLOOR)`:
  only expiry is checked, so a certificate that expired before the floor
  is refused and one issued after it is accepted.
- The seed is drawn before connecting; fewer than 32 random bytes refuses
  the connection with `HttpsError::NoEntropy`.
- Test `certificates_are_checked_at_no_earlier_than_the_floor`.

## Audit of the fix

- H1: the first version checked `notBefore` against the floor as well,
  which on a machine without a clock would have refused every certificate
  renewed after the release (the GitHub update source included):
  `ValidityTime::AtLeast` checks expiry only.
- M1: sshd ignored the result of its random draws too; a short draw now
  makes the boot's keys count as weak (the `ssh entropy weak` report)
  unless a seed was carried.
- L1: the evidence above corrected (the kernel already reports no clock
  for years before 2024).
- L3: the shell says "certificate expired (or the clock is wrong)".
- Open (L2): the floor is raised by hand at each release; deriving it from
  the tagged commit's date, or from a signed time in the update index, is
  a follow-up.
