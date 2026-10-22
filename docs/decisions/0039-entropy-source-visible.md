# ADR 0039: The entropy source is visible

- **Date:** 2026-10-22
- **Status:** Accepted (follow-up of ADR 0034 §4 and incident 0014; amends
  ADR 0025 "Entropy" and ADR 0034 §4)

## Context

The kernel generator (`kernel/src/random.rs`) is ChaCha20 seeded from
`rdrand` when the CPU has it, and otherwise from cycle-counter samples and
the interrupt-timing pool. Only the kernel knew which: its boot line
`random: ChaCha20 generator seeded with rdrand` or `... with TSC (no
rdrand: limited entropy)` was the one sign, readable on the serial console
or a photo of the first boot, never over SSH. netd draws its TCP secret
(ADR 0034) and sshd its host key and per-connection randomness (ADR 0025)
from that generator, and neither could tell a strong seed from a weak one:
netd printed `tcp secret from kernel entropy` in both cases, and ADR 0034
§4 recorded the follow-up "expose the generator's source and seeding state
to user space (a `system_info` kind)". The project's security rule
(CONTRIBUTING.md, "Security") wants real entropy for secrets, and the
failure protocol wants a weak secret to be seen, not assumed away. QEMU's
`qemu64` CPU has no `rdrand`, and whether the reference laptop has it is
still unknown (`docs/hardware.md`).

## Decision

1. **The kernel keeps an account of its generator.** Next to the
   ChaCha20 state, under the same lock, `random.rs` keeps a
   `sysinfo::RandomSource`: the seed's source (`EntropySource::Rdrand` =
   1, `EntropySource::TscInterrupts` = 2), the `rdrand` values mixed in
   (the seed's, up to 8, and one per read since), the cycle-counter
   samples mixed in (64 in the seed, one per read) and the interrupts whose
   timing reached the generator through the pool (counted up to the last
   read). The source is `Rdrand` only if `rdrand` returned plausible data
   for the seed: each of the 8 values is retried up to 10 times while CF =
   0 (Intel's DRNG guide), and the values are counted only if at least two
   distinct ones remain after discarding all-zero and all-one bits, as
   Linux's `x86_init_rdrand` does (some AMD parts return all ones with CF
   set after resume). A CPU that advertises `rdrand` and never delivers,
   or delivers a constant, is reported as `TscInterrupts` with 0 hardware
   samples, its boot line says `rdrand returned no data` or `rdrand values
   rejected: constant or all-0/all-1 bits`, and later reads stop mixing its
   output in; what it returned for the seed is still hashed in, since it
   can only add.
   There is no `Rdseed` value: the kernel does not use `rdseed`, and an ABI
   value nothing produces would be dead code; adding it is additive.
2. **A new additive `system_info` kind, `RANDOM_SOURCE` (12)**, returns
   that account as a 32-byte `RandomSource` (`sdk/abi`, byte 0 the source,
   bytes 8..32 three little-endian `u64`; bytes 1..8 zero). It needs `READ`
   on a `SYSTEM` handle, like every `system_info` kind. It holds only the
   source and counters: no seed, pool word, key or output is ever exposed.
   The struct only grows by appending fields after byte 32; `decode`
   accepts a longer reply and `sys::random_source` passes a 64-byte buffer,
   so a later kernel with a longer struct still answers an older reader
   (a change to the first 32 bytes would need a new kind).
   `sys::random_source(system)` (`sdk/rust`) wraps it and answers `None`
   when the kernel refuses the query or names a source the build does not
   know; callers that draw secrets treat `None` as weak. Host tests: the
   encoding round-trips, unknown sources and short replies are refused, the
   reserved bytes are zero, and `RandomSource::summary` (the wording shared
   by netd, sshd and `diag`) gives `rdrand` or `TSC + interrupts (N
   samples, M events)`. `ABI_VERSION` stays 5: older kinds were added the
   same way.
3. **netd says what its TCP secret is worth.** At start, after drawing
   the secret, it reads `RANDOM_SOURCE` with the clock's `SYSTEM` handle.
   From a hardware source it prints `tcp secret from kernel entropy
   (rdrand)`; otherwise it prints `tcp secret weak: kernel generator seeded
   from TSC + interrupts (N samples, M events), no hardware source: ISNs
   and ports guessable with effort` (or `... the kernel does not report
   what seeded its generator`) and records the fault event `net tcp secret
   weak x1` through its `aricode_faults` meter, code = the source number (0
   when unknown). It keeps running: refusing would leave the machine
   without a network. The `net tcp secret degraded` fault (a refused
   `RANDOM`, ADR 0034 §4) is unchanged; with a degraded secret the strong
   line is never printed.
4. **sshd checks before it draws a host key** (ADR 0025): when it is about
   to generate one (first start, invalid file, or a boot-only key because
   the file cannot be read) and the generator was not seeded from a
   hardware source and no `random_seed` from an earlier boot was read, it
   records `ssh entropy weak` (code = the source number, 0 when unknown)
   and prints `sshd: fault ssh entropy weak: kernel generator seeded from
   ... and no random_seed from an earlier boot: the new host key is weak
   (cargo xtask usb-image --authorized-key installs one made on the build
   host)`. It does not refuse to start, which would lock the owner out;
   the key is drawn as before from the kernel generator and sshd's own
   state, which in exactly this case hold nothing from an earlier boot.
   With a carried seed nothing is reported: every earlier boot added to
   it, and it is mixed in as before.
5. **The owner reads it in `diag`.** The shell's `diag` prints one line
   after the CPUs: `entropy  rdrand`, or `entropy  TSC + interrupts (N
   samples, M events): weak` (`entropy  unknown (...): weak` when the
   kernel does not say), so it can be read over SSH or saved with `diag
   NAME`; the fault events above appear in its event list.
6. **Tests.** `crypto.random` (qemu64) and `crypto.random_rdrand` run the
   kernel self-test on both paths, which now also checks the account
   (source, 67 TSC samples after three reads, the stir count, hardware
   samples present exactly on the `rdrand` path) and the `rdrand`
   plausibility rule (all ones, a constant once all-0/all-1 values are
   dropped, and no values are rejected; two distinct values pass). No QEMU
   CPU model returns a broken `rdrand`, so the rejected boot line itself is
   not exercised in a scenario. `network.stack` and
   `ssh.entropy_weak` assert the weak lines, the `net tcp secret weak x1
   (code 2)` and `ssh entropy weak (code 2)` events and the `diag` line over
   SSH; `ssh.exec` asserts the sshd line. A new scenario option
   `cpu_rdrand = true` boots `-cpu qemu64,+rdrand` (TCG implements
   `rdrand` from the host's randomness), and `crypto.entropy_rdrand` shows
   the hardware path end to end: `seeded with rdrand`, netd's `(rdrand)`
   line, no weak event, `entropy  rdrand` in `diag` over SSH.

## Consequences

- Every QEMU scenario runs the weak path unless it sets `cpu_rdrand`, so
  the event ring of every networked boot holds `net tcp secret weak x1`
  and, on a fresh disk with SSH keys, `ssh entropy weak`. That is the
  truth about `qemu64`, not noise to filter.
- The verdict is about the seed, not about the generator's state now: a
  boot seeded from the TSC gathers interrupt timing as it runs (the
  counters show how much), but nothing measures that entropy, so the line
  stays `weak`. Saving a kernel seed across boots, the deeper fix ADR 0034
  names, is still future work; so is a readiness wait, for which there is
  still no criterion.
- Whether the laptop has `rdrand` is now read from `diag` (over SSH or
  `diag NAME` + `cargo xtask fs-cat`), not from a photo of the boot.
- The counters are a small activity side channel: `tsc_samples - 64` is
  the number of `RANDOM` reads system-wide since boot (every process's, sshd's
  connections included), and `interrupt_events` the number of device,
  keyboard and serial interrupts; `diag` publishes both. They are not
  coarsened because they tell a `SYSTEM` `READ` holder little it cannot
  already read: `IDLE` exposes `tsc_since_boot` and per-CPU idle cycles,
  `EVENTS` and `PROCESSES` the processes and their faults. No program
  without `READ` on `SYSTEM` can query them.
- While wiring netd, a local copy of the 22 KiB `Stack` inside
  `Stack::new` (inlined into `run`) overflowed the driver's 64 KiB stack in
  QEMU; `Stack::new` builds the stack in place and reports the source
  before it. The audit of this branch measured what is left of that stack
  (about 12 KiB in release builds, none in dev builds): incident 0018,
  fixed on its own branch.
