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
  machine without a clock accepted expired certificates; one whose CMOS
  battery is dead reads 2000-01-01 and refused every certificate as not
  yet valid.
- Seed (C-L9): the key exchange's seed came from `sys::random` with its
  result ignored: with a bad handle or too few bytes, the X25519 secret was
  all zeros, a key anyone can compute, with no error.

## Fix

- `aricode_x509::check_time` gives the clock's time, never earlier than
  `TIME_FLOOR` (the release day, raised at each release, docs/WORKFLOW.md):
  dates are always checked; a certificate that expired before the floor is
  refused whatever the clock says, and a clock reset to 2000 no longer
  refuses every valid certificate.
- The seed is drawn before connecting; fewer than 32 random bytes refuses
  the connection with `HttpsError::NoEntropy`.
- Test `certificates_are_checked_at_no_earlier_than_the_floor`.
