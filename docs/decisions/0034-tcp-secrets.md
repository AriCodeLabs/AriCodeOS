# ADR 0034: TCP secrets: initial sequence numbers and ephemeral ports

- **Date:** 2026-10-18
- **Status:** Accepted (incident 0014; amends ADR 0024); §4 amended by
  ADR 0039 on 2026-10-22 (the generator's source is visible)

## Context

The network service chose three numbers that RFC 6528 and RFC 6056 want
unpredictable as fixed functions of public inputs: the initial sequence
number of an active connection was `uptime_ms * 64 + slot`, the one of a
passive connection a fixed multiplication of the peer's own sequence
number, and the ephemeral ports started at 50000 on every boot and
increased by one per connection. Incident 0014 describes the
consequences: an off-path host on the LAN can guess the 4-tuple and the
sequence window of an SSH or HTTPS connection with few tries and inject
an RST or data (RFC 5961), and after a reboot the machine reuses the
4-tuples of the previous boot against the same servers with a lower ISN,
which a server or a NAT still holding the old connection may answer with
an RST (the likely one-off of incident 0012).

The project's security rule ([CONTRIBUTING.md](../../CONTRIBUTING.md),
"Security") requires real entropy for secrets, and an ISN is a secret in
the sense of RFC 6528.

## Decision

1. **One secret per boot.** `Stack::new` draws `isn::SECRET_LEN` (32)
   bytes from the kernel generator (`sys::random`, `system_info(RANDOM)`)
   with the `SYSTEM` handle the driver already holds for its clock: init
   duplicates it with `READ | TRANSFER` for every driver launched
   `with_system` (`services/init/src/main.rs`), the kernel checks `READ`
   for `system_info`, and `TRANSFER` only let init hand it over. No change
   to the handles init gives virtio-net or rtl-net. The kernel generator is
   ChaCha20 seeded from `rdrand`, or from TSC samples and the
   interrupt-timing pool on CPUs without it, the same source sshd draws
   its per-connection randomness from (ADR 0025). The secret lives only in
   netd's memory and is never written anywhere.
2. **ISN = M + F(local_ip, local_port, remote_ip, remote_port, secret)**
   (RFC 6528 section 3), in `aricode_net::isn::initial_seq`: `M` is the
   millisecond clock in 4 µs ticks (`now_ms * 250` mod 2^32) and `F` the
   first 32 bits of HMAC-SHA-256 (`aricode_crypto::hmac`) of a tag byte and
   the 4-tuple under the secret. Active opens use it for the SYN, passive
   opens for the SYN|ACK: `tcp::Connection::listen(iss)` now takes the ISN
   from the caller, computed when the SYN arrives, and the connection never
   derives one from the clock or the peer's number. The same tuple gets
   ISNs that only grow with the clock within a boot (so a quick reconnection
   stays ahead of the old connection's numbers, as RFC 6528 intends), and
   unrelated ISNs across boots or tuples.
3. **Ephemeral ports** (RFC 6056 section 3.3.3, `isn::ephemeral_port`):
   `port = 49152 + ((F'(local_ip, remote_ip, remote_port, secret) + count)
   mod 16384)`, the IANA range, with a counter of connections opened this
   boot and a second tag byte for `F'`. Each destination gets its own walk
   and every boot starts somewhere else. `isn::pick_ephemeral_port` (pure,
   host-tested) walks the candidates from the counter and skips the ones a
   caller-supplied check marks in use; netd marks a port held by any TCP
   slot, listeners and TIME-WAIT included (a slot stays until `tcp_poll`
   frees it). With 8 slots at most 8 ports are in use and consecutive
   candidates are distinct, so the answer comes within 9 candidates; the
   function gives up with `None` after one pass over the range, which netd
   answers as `Busy` (`net tcp table full`).
4. **Degradation is loud, weakness too.**
   `system_info(RANDOM)` always answers with `min(len, 256)` bytes
   (`kernel/src/user/syscalls.rs`), so the fault event `net tcp secret
   degraded` (code = bytes obtained) and the console line `fault net tcp
   secret degraded: too few random bytes, ISNs predictable` fire only when
   the syscall itself is refused (a handle without `READ`, or a kernel
   without the query); the rest of the secret is then filled from the
   clock, so that boot is as predictable as before and `diag` says so.
   The quality of the bytes is a separate question, answered since ADR
   0039 by `system_info(RANDOM_SOURCE)`: right after drawing the secret
   netd asks what seeded the kernel generator. From `rdrand` it prints
   `tcp secret from kernel entropy (rdrand)`; from the TSC and interrupt
   timing only (or when the kernel does not say) it prints `tcp secret
   weak: kernel generator seeded from TSC + interrupts (N samples, M
   events), no hardware source: ISNs and ports guessable with effort` and
   records `net tcp secret weak x1` through its fault meter, code = the
   `EntropySource` number (2; 0 when unknown). netd runs either way. QEMU's
   default CPU has no `rdrand`: scenario `network.stack` requires the weak
   line and event and forbids the degraded one; `crypto.entropy_rdrand`
   (`-cpu qemu64,+rdrand`) requires the strong line and no weak event.
   Incident 0014's plan item 3 (wait for entropy, `net isn no entropy`)
   stays superseded: the kernel reports a source, not a readiness state,
   so there is still nothing to wait for. Saving a kernel seed across
   boots, future work when this was written, is ADR 0043: with a carried
   seed mixed in netd prints `tcp secret from kernel entropy (TSC +
   interrupts + carried seed)` and records no weak event.
5. **Host tests** (`libs/net`): the ISN differs across secrets for one
   tuple and across tuples for one secret (by more than 2^16, never a
   neighbour), grows by 250 per millisecond for one tuple and secret and
   wraps with period 2^31 ms; the SYN|ACK of a passive open carries the
   caller's ISN whatever the peer's sequence number; ports stay in range,
   walk by one per count, differ per destination and start at distinct
   ports for distinct secrets, never 50000. Each test fails on the formula
   it replaces (checked by mutation while writing them).

## Consequences

- `aricode-net` depends on `aricode-crypto` (both `no_std`, no `alloc`,
  no external crates). One HMAC-SHA-256 per connection attempt (four
  compressions) and one per candidate port; negligible next to the
  handshake.
- The secret changes at every boot. After a reboot the first port towards
  a destination is uniform over the 16384 of the range, so a 4-tuple the
  server still holds (`k` old connections to that destination) is reused
  with probability about `k / 16384` per connection, and the new ISN is
  then unrelated to the old one (lower about one time in two). The
  collision of incident 0012 candidate 4 goes from certain (same port,
  lower ISN) to improbable, not to impossible: a `Reset` right after a boot
  is still possible and is not excluded a priori.
- The ISN clock is 4 µs from a 1 ms source: within one millisecond the
  same tuple would get the same ISN. netd never opens two connections
  with the same 4-tuple (the port search skips ports in use), so this is
  moot for active opens; a peer that re-sends a SYN within a millisecond
  to a listener creates one passive connection, not two.
- The secret is not persisted or rotated during a boot; a boot that lasts
  months keeps one secret (RFC 6528 section 3 allows this while the system
  is not rebooted; rotation would need care with connections in
  TIME-WAIT). Not planned.
- On a CPU without `rdrand`, the quality of the secret is that of the
  kernel generator at netd's start (interrupt-timing pool plus TSC); sshd's
  persisted seed does not help netd, which has no file service. The
  kernel's generator, not each service, is where the entropy question is
  answered (ADR 0025, "Entropy"). Its source is visible since ADR 0039
  (`net tcp secret weak` when it is not a hardware one). Since ADR 0043
  AriBoot hands the kernel a seed carried from the previous boot before
  any process runs, so netd's secret also holds what earlier boots
  gathered; only the first boot of a disk is weak. Whether the laptop has
  `rdrand` is read from the `entropy` line of `diag` (`docs/hardware.md`).
