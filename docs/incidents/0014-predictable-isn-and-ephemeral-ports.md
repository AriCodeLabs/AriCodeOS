# Incident 0014: predictable TCP initial sequence numbers and ephemeral ports

- Opened: 2026-10-18. Status: open (security), found by reading.
- Found by: the independent audit of `fix/net-refusal-visibility`
  (incident 0012), while weighing candidate 4 (an RST from the other end
  after a reboot).
- Code: `services/net-core/src/lib.rs` (`TcpConnect`: `conn.connect((now
  as u32).wrapping_mul(64).wrapping_add(slot as u32), now)`,
  `tcp_next_port` starting at `TCP_EPHEMERAL_FIRST = 50000`);
  `libs/net/src/tcp.rs` (`on_segment`, `Listen` arm: `iss =
  h.seq.wrapping_mul(2654435761).wrapping_add(0x4152_4900)`).

## Observation

Three values that RFC 6528 and RFC 6056 want unpredictable are fixed
functions of public inputs:

1. **Active ISN** = `uptime_ms * 64 + slot`. Anyone who knows roughly how
   long the machine has been up (the SSH banner, a ping RTT pattern, the
   DHCP lease time) knows the ISN of its next connection to within a few
   thousand, and the first connections after a boot start near zero.
2. **Passive ISN** (sshd's connections) = a fixed multiplication of the
   peer's own sequence number. The peer chooses `h.seq`, so the peer
   chooses our ISN; anyone else who sees the SYN computes it.
3. **Ephemeral ports** start at 50000 after every boot and increase by one
   per `TcpConnect`; the N-th connection of every boot uses port
   `50000 + N - 1`.

## Consequences

- **Off-path injection on the LAN** (RFC 6528 section 1, RFC 5961). A host
  on the same network that sees or guesses a 4-tuple can inject data or an
  RST into an SSH or HTTPS connection with few guesses: the sequence
  window of a 4 KiB receive buffer against an ISN known to within
  thousands. TLS and SSH detect injected data (the record MAC fails), but
  the connection is torn down; an injected RST needs no MAC. The
  predictability also makes a blind spoofed connection to the SSH listener
  feasible for a LAN attacker, which the host-key and user-key checks then
  reject, but after taking a TCP slot (incident 0013).
- **4-tuple collision after a reboot** (incident 0012 candidate 4). Minutes
  after a reboot the machine reuses the ports of the previous boot against
  the same servers (`github.com:443` via the updater and the shell), with
  an ISN lower than the one the server last saw. A server or a NAT still
  holding the old connection (TIME-WAIT on its side, a mapping that
  survived our reboot) may answer the new SYN with an RST or a challenge
  ACK (RFC 793 section 3.4, RFC 5961 section 4): `TcpConnect` ends in
  `Status::Reset`, which until incident 0012 the shell printed as "network
  service rejected the request". Consistent with the laptop observation:
  once, right after the boot, never again (the next attempt used the next
  port).

## Rule

The security rule of the project (set by the maintainer on 2026-10-11:
services closed by default, no committed keys, refusals logged, **real
entropy for secrets**; applied in ADR 0025 for the SSH keys): an ISN is a secret
in the sense of RFC 6528 ("the ISN must be unpredictable to off-path
attackers"); it must come from the kernel's entropy (`crypto.random`, the
source sshd already uses for its keys), not from the clock or the peer.

## Plan (no change in this branch)

1. **ISN** = `M + F(local_ip, local_port, remote_ip, remote_port, secret)`
   as RFC 6528 section 3 prescribes: `M` a 4 µs clock (`uptime_us / 4`),
   `F` a keyed hash (the project has SHA-256 and ChaCha20 in
   `libs/crypto`; a SipHash-like or the first 32 bits of SHA-256 over the
   tuple and a 128-bit boot secret suffice) and the secret drawn once per
   boot from the kernel's entropy. Same function for active and passive
   connections; the passive one stops depending on the peer's number.
2. **Ephemeral ports**: a random start in `[50000, 65535]` per boot and the
   RFC 6056 section 3.3.3 double-hash increment, or at least a random
   start and a sequential walk; never the same first port on two boots.
3. The secret is fetched from `SYSTEM` through the existing randomness
   syscall at `Stack::new`; if the kernel has no entropy yet (early boot,
   a machine without `rdrand`), the stack waits for it before the first
   `TcpConnect`, as sshd waits for its seed, instead of falling back to the
   clock. The wait is reported (`net isn no entropy`, a fault event) and
   bounded.
4. Host tests in `libs/net/src/tcp.rs`: two connections with the same
   4-tuple and different secrets get unrelated ISNs; the same secret and
   tuple get ISNs that differ by the clock only; a passive ISN does not
   change with the peer's sequence number.
5. The reboot collision of incident 0012 is then no longer reproducible
   by design; the incident stays open until the laptop confirms the
   mechanism or the rule above makes it moot.
