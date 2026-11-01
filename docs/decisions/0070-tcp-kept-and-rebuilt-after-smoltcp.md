# ADR 0070: TCP kept, its transmission rebuilt after comparing it with smoltcp

- **Date:** 2026-11-01
- **Status:** Accepted. Step 6 of ADR 0063's order of work, TCP/IP part
  (the WASM interpreter follows). Fixes incident 0056.

## Context

ADR 0063 asks for the comparison of the network service's TCP
(`aricode-net::tcp`, ADR 0024, ADR 0034, ADR 0035) with smoltcp.
smoltcp 0.14.0 (2026-08-07) is the reference small TCP/IP stack in Rust:
- `no_std`, with or without `alloc`;
- under 0BSD, on crates.io;
- used by many embedded and hobby systems.

Its changelog since 0.12 shows what such a stack gets wrong. Remote
panics were fixed in 2024 to 2026:
- a SYN from 0.0.0.0;
- SACK overflow;
- DHCP lease times;
- the DNS server list.

So were RFC 5961 details: challenge ACKs, and out-of-window handling. One
of those fixes (0.14, #1162) stopped rate-limiting the ACKs of
out-of-window data, because the limit stranded peers in retransmission.

## Measurements

### Method

A scratch harness (`aricode-scratch/tcp-compare`) drives each
implementation through the same interface (IPv4 packets in, packets out,
a clock). It has three modes:
- **Simulated link:** 100 Mbit/s, round trip 1, 20 or 80 ms, drop-tail
  queue of 100 packets, random loss and reordering. Each case moves 4 MiB
  and takes the median of five seeds; time is simulated, so the figure is
  the protocol's.
- **Linux:** the kernel's TCP through a TUN device in a user network
  namespace, 16 MiB each way, with the harness dropping packets.
- **Scripted peer:** segments built by hand against an established
  connection, and the stack's answer.

smoltcp ran through its `Interface` in `Medium::Ip`. Its TCP socket cannot
be driven alone (`process` and `dispatch` are crate-private).

### Robustness and blind attacks

| Segment or condition | Before | smoltcp 0.14 | After |
|---|---|---|---|
| RST far outside the window | closed | ignored | ignored |
| RST in the window, not exact (RFC 5961 3.2) | closed | closed | challenge ACK |
| RST at the exact next sequence number | closed | closed | closed |
| SYN on an established connection | ignored | ignored | challenge ACK |
| data with an ACK of bytes never sent | delivered | dropped | dropped |
| data with an ACK a million bytes old | delivered | dropped | dropped |
| a segment ahead of a hole | dropped | kept | kept |
| the peer's MSS of 536 | 1400 sent | 536 | 536 |
| zero window, window update lost | stalls forever | 4 probes in 30 s | 8 probes in 30 s |
| RST in SYN-SENT not acknowledging our SYN | believed | ignored | ignored |

### Initial sequence numbers

smoltcp draws ISNs from an sPCG32 generator, a linear congruential
generator seeded with `Config::random_seed`, and offers no way to supply
them. Two observed ISNs gave the next one in 0.7 s on one core of the
build host, in three runs out of three (`isn` mode of the harness). An
off-path attacker who opens two connections can predict a third
connection's sequence numbers, the attack RFC 6528 exists to stop.

This system's ISNs are SipHash of the address and port pair under a
per-boot secret, plus a clock (ADR 0034).

### Transfers, simulated link

Both stacks with 32 KiB to send and 64 KiB to receive, MB/s, median of
five seeds (minimum in brackets where it differs much). The last column is
the code as merged, after the audit's fixes:

| Round trip, loss, reordering | Before | smoltcp, no congestion control | smoltcp, Reno | After |
|---|---|---|---|---|
| 1 ms, none | 11.77 | 11.80 | 11.34 | 11.77 |
| 1 ms, 0.5 % | 0.003 | 11.17 (3.03) | 2.98 | 10.76 |
| 1 ms, 2 % | 0.003 | 0.56 | 0.18 | 4.96 (2.88) |
| 1 ms, 1 % reordered | 0.003 | 11.70 | 7.99 | 10.99 |
| 1 ms, 1 % loss + 1 % reordered | 0.003 | 1.74 | 0.39 | 9.17 (4.90) |
| 20 ms, none | 1.62 | 1.56 | 1.48 | 1.61 |
| 20 ms, 0.5 % | 0.003 | 1.07 | 0.74 | 1.04 |
| 20 ms, 2 % | 0.003 | 0.39 | 0.14 | 0.58 |
| 20 ms, 1 % reordered | 0.003 | 1.51 | 0.67 | 0.88 |
| 80 ms, 0.5 % | 0.003 | 0.36 | 0.19 | 0.26 |
| 80 ms, 2 % | 0.002 | 0.21 | 0.08 | 0.14 |
| 80 ms, 1 % reordered | 0.002 | 0.39 | 0.18 | 0.22 |

- With congestion control on both sides, the comparison RFC 5681 asks
  for, the new implementation is ahead of smoltcp's Reno in every row.
- smoltcp's default is no congestion control: it keeps sending the whole
  window through loss. That wins in this model with long round trips and
  with reordering, where the new implementation halves its window on a
  fast retransmit the reordering caused.
- smoltcp's CUBIC does not build without `std` (`f64::powi`).

### Transfers, Linux through TUN

| | Before | After |
|---|---|---|
| Linux to us, no loss | 320 MB/s | 603 MB/s |
| Linux to us, 0.5 % loss | 0.36 MB/s | 329 MB/s |
| Linux to us, 2 % loss | 0.07 MB/s | 22.0 MB/s |
| us to Linux, no loss | 8.2 MB/s | 8.2 MB/s |
| us to Linux, 0.5 % loss | 0.26 MB/s | 7.5 MB/s |
| us to Linux, 2 % loss | 0.23 MB/s (32 KiB buffers) | 1.8 MB/s |

Every transfer arrived whole and in order (a position pattern is checked).
Sending to Linux is held near 8 MB/s by the harness's single-threaded
loop over TUN, the same with no loss before and after.

### Inside AriCodeOS

`bench` (perf scenario, QEMU TCG, one CPU) gained two figures:
- a 1 MiB download from the host test server's `/bulk`;
- 256 KiB echoed by its TCP echo server in 16 KiB rounds.

Both builds have the same `bench` and the same server. Each figure is the
median of five transfers; the range covers the runs of the perf area alone
(two before, three after the audit's fixes):

| | Before | After |
|---|---|---|
| Download 1 MiB | 119 ms | 99 to 109 ms |
| Echo 256 KiB | 641 to 801 ms | 411 to 606 ms |

QEMU's user networking loses nothing, so these measure the stack's own
cost. A client still receives at most 1472 bytes per request, so the
download is bounded by IPC, not by TCP.

A first build of the new stack took 706 ms for the echo. QEMU's slirp
holds a short segment back until its earlier data is acknowledged
(Nagle), and the new delayed ACK waited 20 ms for a second segment.
`recv` now sends a delayed ACK at once when the reader has taken
everything, as Linux's `tcp_cleanup_rbuf` does.

## Decision

1. **The TCP stays this system's; smoltcp is not adopted.**
   - Its ISNs are predictable, and only a fork could change that, which
     ADR 0063 rules out.
   - It accepts any reset in the window.
   - Adopting it would mean its whole `Interface` beside netd's ARP,
     IPv4, DHCP and DNS, and giving up the listener protection of
     ADR 0035 or rebuilding it around the interface.
2. **The transmission and reception are rebuilt** (`libs/net/src/tcp.rs`,
   incident 0056), taking what smoltcp, Linux and the RFCs do:
   - **timeout:** RFC 6298 with Karn's rule, floored at 200 ms (Linux);
   - **after a timeout:** go-back-N, with the window collapsed to one
     segment;
   - **congestion control:** NewReno (RFC 5681, RFC 6582), an initial
     window of ten segments (RFC 6928), byte counting in slow start;
   - **SACK (RFC 2018):** out-of-order data is kept in up to four ranges
     and reported. The holes to resend come from the peer's blocks, one
     per duplicate ACK;
   - **the peer:** its MSS is honoured, and a closed window is probed
     with an old sequence number, as Linux does;
   - **ACKs:** one for every second segment, and at most 20 ms late;
   - **RFC 5961:** a reset closes only at the exact next sequence number.
     Within the window, a reset, a SYN or an impossible ACK gets a
     challenge ACK, at most one a second. Out-of-window data is
     acknowledged at once (smoltcp #1162);
   - **give-up:** a time, `GIVE_UP_MS`, instead of a count, so a short
     measured RTO keeps ADR 0035's 31 s.
3. **Buffers come from the caller.** netd lends each connection 32 KiB to
   send and 64 KiB to receive, from a pool in zero-fill memory: 768 KiB
   reserved, backed only once a connection writes to it. The runtime's
   256 KiB heap could not hold them. A listener borrows none, and
   dropping a socket returns its pair.

## Consequences

- Downloads (the updater, `http`, HTTPS) no longer collapse on a lossy
  network, and a blind attacker can no longer reset sshd's connection
  knowing only its ports.
- netd's `.bss` grows by 768 KiB of reserved memory, backed only once
  used; its code grows by 7.5 KiB (virtio-net: 94 337 to 101 825 bytes
  of text).
- The receive window of 64 KiB, without window scaling, bounds one
  connection to 64 KiB per round trip: 3.2 MB/s at 20 ms.

## Limits

- **No window scaling or timestamps** (RFC 7323): larger windows would
  need larger buffers than the pool holds.
- **Reordering.** A reordering of three segments or more triggers a fast
  retransmit and halves the window (DSACK undo, RFC 2883 and RFC 3708, is
  not implemented).
- **No simultaneous open** (RFC 9293 MUST-10).
- **SACK is kept in four ranges** each way. A fifth hole is recovered by
  the timeout.
- **Old segments lose their ACK field.** A segment entirely below
  `rcv_nxt`, as a peer's go-back-N sends, is answered but its ACK is not
  used (RFC 9293). Linux takes it; doing the same would widen what a blind
  segment can touch.

## Tests

- Host (`libs/net/src/tcp.rs`, 43 tests): the earlier schedules, and:
  - each rule of the robustness table;
  - fast retransmit, SACK recovery and go-back-N;
  - RTT estimation;
  - delayed ACKs;
  - a FIN ahead of a hole;
  - TIME-WAIT re-acknowledging;
  - a 1 MiB transfer through 1 % and 5 % loss;
  - malformed options;
  - random segments in every state (20 000 per run, `TCP_FUZZ_ITERATIONS`);
  - both directions over a wire that delays, reorders, duplicates and
    loses up to 14 % (60 seeds per run, 1000 once with
    `TCP_WIRE_SEEDS`).
- The harness's robustness table and transfers above, after the audit.
- QEMU: the `network`, `ssh`, `perf` and `update` areas, 69 scenarios.

## Audit (2026-11-01)

An independent audit found no medium findings. It fuzzed:
- 40 000 seeds of segments injected near the sequence numbers of a live
  pair;
- 600 transfers over a lossy wire that reorders and duplicates.

It found no panic, no stream corruption and no timer missing from
`next_timer`. It checked:
- that pool buffers reused across connections are never read before
  they are written;
- that every path that drops a socket returns its buffers;
- the `unsafe` split of the pool;
- the RFC 6298 and RFC 6582 arithmetic;
- the schedules of ADR 0035.

Applied:
- **H1 (high).** After a timeout, pure ACKs went out at the rewound
  `snd_nxt`, below what the peer had received. The peer dropped them as
  out of window, and with data both ways both sides resent into each
  other until one gave up (41 of 200 seeds of the audit's wire).
  - ACKs and resets now go at `snd_max`, as in BSD.
  - A zero-length segment at the window's right edge is acceptable, as
    Linux takes it.
  - `both_directions_survive_a_bad_wire` fails without the fix (seed 23).
- **L1.** A close in SYN-RECEIVED followed by a timeout sent a FIN at the
  ISN. The FIN now waits for the SYN to be acknowledged, and the SYN|ACK
  keeps its budget.
- **L2.** A resend owed by a partial ACK survived the full ACK; it is
  cleared, and served only during recovery.
- **L3.** The resend after a timeout left no room for SACK blocks and
  could exceed the peer's MSS.
- **L4.** After netd delivered data to a client, the window update waited
  for the next wakeup; the connection is now polled again.
- **L5.** A FIN alone into a full buffer was refused; it needs no room.
- SACK blocks are bounded across several options, and the latest block
  stays first after trimming.

Recorded: L6 under Limits; an Established connection whose peer keeps
answering window probes lives on (standard; the closing states have their
30 s bound).
