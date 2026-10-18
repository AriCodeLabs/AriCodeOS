# ADR 0034: TCP secrets: initial sequence numbers and ephemeral ports

- **Date:** 2026-10-18
- **Status:** Accepted (incident 0014; amends ADR 0024)

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
   with the `SYSTEM` (`READ`) handle the driver already holds for its
   clock: no change to the handles init gives virtio-net or rtl-net. The
   kernel generator is ChaCha20 seeded from `rdrand`, or from TSC samples
   and the interrupt-timing pool on CPUs without it, the same source sshd
   draws its per-connection randomness from (ADR 0025). The secret lives
   only in netd's memory and is never written anywhere.
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
   and every boot starts somewhere else; netd skips a candidate held by any
   TCP slot, listeners and TIME-WAIT included (a slot stays until
   `tcp_poll` frees it), by trying the next count. With 8 slots at most 8
   ports are in use and 9 consecutive candidates are distinct, so the
   search ends.
4. **Degradation is loud.** If the kernel gives fewer than 32 bytes or
   refuses the read, netd records the fault event `net tcp secret
   degraded` (code = bytes obtained), prints `fault net tcp secret
   degraded: too few random bytes, ISNs predictable` and fills the rest
   from the clock, so that boot is as predictable as before and `diag`
   says so; it never falls back silently. On a normal start it prints `tcp
   secret from kernel entropy`, which scenario `network.stack` requires,
   together with the absence of the degraded line.
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
- The secret changes at every boot, so a connection of the previous boot
  that the peer still holds sees a SYN with an unrelated ISN and port:
  RFC 793 section 3.4 then applies as for any new connection, and the
  collision of incident 0012 candidate 4 cannot recur by construction.
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
  answered (ADR 0025, "Entropy"); a deeper fix there (saving a seed across
  boots at kernel level) is future work.
