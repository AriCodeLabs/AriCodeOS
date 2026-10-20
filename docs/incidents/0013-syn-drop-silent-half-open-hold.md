# Incident 0013: silent SYN drop and half-open connections holding the TCP table

- Opened: 2026-10-18. Status: fixed 2026-10-19 (ADR 0035,
  `fix/tcp-syn-drop-and-half-open`); found by reading, mechanism confirmed
  by host tests and reproduced in part under QEMU (below).
- Found by: the independent audit of `fix/net-refusal-visibility`
  (incident 0012), while checking that every refusal of netd is recorded.
- Code: `services/net-core/src/lib.rs`, `on_tcp` (a SYN to a listening
  port with no free slot) and `tcp_poll`; `libs/net/src/tcp.rs`, the
  SYN-ACK retransmission of a passive connection.

## Observation

Two gaps in the same path:

1. **The one refusal still unrecorded.** When a SYN arrives for a listening
   port and the TCP table (`MAX_TCP = 8`) has no free slot, `on_tcp`
   returns without a word: no RST to the peer, no fault event, no console
   line. The peer retransmits its SYN until it gives up; nobody on the
   machine learns that connections were turned away. Every other refusal
   now leaves a `net ...` fault event; this one, a refusal of a remote
   client rather than of a local one, does not.
2. **Half-open connections keep their slot for about 31 s.** Every SYN to a
   listening port creates a `SynReceived` socket at once (`on_tcp` ->
   `Connection::listen` + `on_segment`), before anyone calls `TcpAccept`.
   The SYN-ACK is retransmitted with the data RTO schedule (0.5, 1, 2, 4,
   8, 8, 8 s; 6 retries, `libs/net/src/tcp.rs`) before the connection is
   closed for `CloseReason::Timeout`, so a SYN that is never completed
   holds a slot for roughly 31 s. With sshd listening (one slot for the
   listener), seven stray SYNs every 30 s keep the table full for
   everyone: the shell's `TcpConnect`, the updater, the next SSH login.
   There is no cap on half-open connections per listener and no shorter
   budget for an unanswered SYN-ACK.

## Why it matters for incident 0012

Candidate 5 of incident 0012: a `Busy` on the laptop's `http` right after a
reboot fits a table filled by SYNs that nobody completed. SSH clients on
the LAN that lost their session in the reboot retry; a router or a
scanner probing port 22 does the same. Nothing of this is visible today:
`diag` shows no `SynReceived` count, the dropped SYNs leave no event and
the shell only sees `Busy` (now named).

## What the evidence can tell

Nothing yet: the laptop observation predates the new events and no capture
of the laptop's LAN exists. The scenario `network.tcp` exercises a single
accepted connection and the host probe; no scenario sends SYNs that are
never completed or more SYNs than slots.

## Mechanism (confirmed)

Host tests of `libs/net` reproduce both halves on the pure state machine
before the fix (written first, failing on the old constants):

- `unanswered_syn_ack_gives_up_within_the_short_budget`: a `Connection`
  in `Listen` that receives a SYN and never an ACK stayed `SynReceived`
  until the data schedule ran out, 31.5 s (0.5 + 1 + 2 + 4 + 8 + 8 + 8 s),
  holding its slot all that time. The hold is the schedule, not a leak.
- `on_tcp` had three silent exits for a SYN: no listener (unchanged, the
  port is closed by default), no free slot and, implicitly, no bound at
  all on how many connections one listener could create. With sshd
  listening, seven SYNs filled the seven other slots and the eighth and
  every later one left without a trace; a `TcpConnect` meanwhile got
  `Busy` (`net tcp table full`), which named the table but not who held
  it.

Under QEMU a half-open flood cannot be sent: user networking (`hostfwd`)
completes every handshake itself. What can be reproduced is the other
holder of a listener's slots, established connections nobody accepts:
eight plain connections from the host to sshd's port, held without a
byte. Before the fix they took every free slot (sshd serves one at a time,
so seven queued) and the shell's `http` was refused with `busy`.

## Fix (ADR 0035)

- `libs/net/src/tcp.rs`: `SYN_ACK_MAX_RETRIES = 3` for `SynReceived`
  (0.5, 1, 2 s and a last 4 s wait: about 7.5 s, then `Timeout` and the
  slot is free); `MAX_RETRIES = 6` unchanged for active opens and data
  (pinned by `active_open_and_data_keep_the_full_schedule`);
  `listen_backlog(slots) = slots / 2`; `reset_for` (RFC 793 section 3.4).
- `services/net-core`: a SYN to a listener whose backlog (connections with
  `accepted_from` set, half-open or established) is at
  `TCP_BACKLOG = 4`, or with no free slot, is answered with the reset and
  recorded as `Refusal::SynDropped` (`net syn dropped x<count>`, code 5,
  through incident 0012's meter); closing a listener resets and frees its
  backlog instead of orphaning it; `Op::TcpStats` returns the table by
  state and the SYNs reset since boot.
- `sdk/proto`, `sdk/rust`: `Op::TcpStats`, `TcpStats`,
  `NetClient::tcp_stats`.
- `services/shell`: `net` (and `diag`, which prints it) ends with `tcp
  8 slots: 1 listen, 0 syn-rcvd, 0 syn-sent, 3 backlog, 1 established, 0
  closing, 0 time-wait, 3 free; 4 SYNs reset`.
- Audit of the fix (2026-10-19), three corrections in `on_tcp`: a segment
  carrying RST is ignored before any listener lookup (RFC 793 section 3.4
  never answers a reset with a reset, and a crafted SYN|RST, nmap's
  `--scanflags SYNRST`, would have created a child that stays in LISTEN,
  arms no timer, counts towards the backlog and was never reaped: four of
  them would have shut sshd out until reboot); a SYN from a source that
  cannot be a host (unspecified, multicast, limited or subnet broadcast,
  RFC 1122 section 3.2.1.3; `Ipv4Addr::is_invalid_source`) is dropped, not
  reset; and the reset is stateless, sent only when the next hop's MAC is
  already known, so a spoofed on-subnet flood cannot turn into ARP
  broadcasts and `net arp defer full` for legitimate traffic. The hand-out
  loop of `tcp_poll` now reaps any child of a listener that is neither
  half-open, established nor finished (defence in depth behind the RST
  check). The crafted flags cannot be sent through QEMU's user networking,
  so the regression is the host test `listener_ignores_a_syn_carrying_rst`
  and the code path, not a scenario.
- `tools/xtask`: `[host_flood]` in a scenario opens N plain connections to
  the guest's forwarded port 22 or 7 on a marker, holds them until QEMU
  exits counting the ones the guest resets, and holds the console lines
  until they are open (`tests/README.md`).

## Tests

- Host (`cargo xtask test --level unit`): six `libs/net` TCP tests
  (`unanswered_syn_ack_gives_up_within_the_short_budget`,
  `active_open_and_data_keep_the_full_schedule`,
  `answered_syn_ack_resets_the_retry_count`,
  `listen_backlog_leaves_half_the_table_to_active_opens`,
  `reset_for_follows_rfc_793_section_3_4`,
  `listener_ignores_a_syn_carrying_rst`) and
  `invalid_sources_are_recognised`; `aricode-proto` round-trips
  `TcpStats`; xtask's flood probe against a local listener.
- QEMU `network.listen_backlog` (full, areas network/diag/ssh): proves
  that with eight held connections to sshd the backlog stops at 4, the
  SYNs beyond it are reset and recorded (`virtio-net: fault net syn
  dropped x1`, `diag`: `net syn dropped x1 (code 5)`), the shell's `http`
  succeeds meanwhile (`HTTP 200`), `diag` shows the table with the
  listener and never `0 free`, and the host sees its surplus connections
  reset within two seconds of opening them. It does not prove the half-open budget (host tests do) nor
  behaviour towards a spoofed source (a reset of 40 bytes per SYN, no
  state: ADR 0035 argues it, nothing measures it).

Evidence from the scenario (QEMU, `build/scenarios/network.listen_backlog/
serial.log`, 2026-10-19): the eight connections arrived while sshd already
held its accept, so one became sshd's session, four filled the backlog and
three were reset; the first refusal was recorded at once and the two held
back by the meter when their second ended.

```text
sshd: connection from 10.0.2.2
ari> virtio-net: fault net syn dropped x1
HTTP 200, 19 bytes
Hola desde el host
virtio-net: fault net syn dropped x2
tcp     8 slots: 1 listen, 0 syn-rcvd, 0 syn-sent, 4 backlog, 1 established, 0 closing, 0 time-wait, 2 free; 3 SYNs reset
  [11.851 s] fault reported by process 10: net syn dropped x1 (code 5)
  [12.875 s] fault reported by process 10: net syn dropped x2 (code 5)
[host] flood: 8 connections to guest port 22 opened
[host] flood: 3 of 8 connections reset by the guest within 2000 ms
```

`ssh.exec` (an idle client plus the real one, both within the cap) and
`ssh.shell_fault` are unchanged and green with the cap in place.

## Open

- A flood from a spoofed or unreachable source still costs a slot for
  7.5 s per SYN admitted: about four SYNs every 7.5 s keep one listener's
  backlog full, which costs the attacker nothing and the machine 4 of its
  8 slots. Active opens keep working; the owner's own SSH login does not
  while it lasts. SYN cookies (RFC 4987) would remove the slot cost but
  need the connection to be created on the ACK, which this stack does not
  do; left for when it is observed.
- The cap is per listener; a second listening service would let two
  backlogs take the whole table (ADR 0035).
- Receive budget per round: fixed 2026-10-20 in `fix/net-rx-budget`
  (ADR 0035, note). The main loop used to drain every frame the NIC held
  (`while let Some(n) = stack.link.recv_frame(..)`) before serving IPC, so
  a line-rate SYN flood starved the clients' requests, at the cost of a
  transmit attempt (the reset) per SYN too. A round now receives at most
  `aricode_net::rx::FRAMES_PER_ROUND = 32` frames (`libs/net/src/rx.rs`,
  host-tested: the budget stops a flood and reports the frames left, a
  quiet ring is drained silently), then serves its eight IPC requests and
  the timers, and starts the next round at once (no interrupt wait) when
  frames were left, recording `net rx budget hit xN` (code 0) through the
  meter. `Link::rx_pending` (virtio: `Virtqueue::has_used`; rtl: the next
  descriptor's `OWN` bit) says whether frames were left without taking
  one. What a client waits under a flood is bounded to one round. No
  scenario can send a flood through QEMU's user networking; the budget
  decision is host-tested and `network.*`, `net.https` and `update.*`
  keep their timings.
- A reset is not sent when the next hop's MAC is unknown (by design, see
  above); a real first-contact peer learns of the refusal on its SYN
  retransmission, after one RTO.
- `TcpStats` counts a boot's refusals in `syn_dropped` but not per
  listener or per source; a capture is still needed to know who floods.
- The laptop observation of incident 0012 is still unexplained; the new
  `diag` line will say, on the next `Busy`, whether a backlog held the
  table.

## Plan (as written on 2026-10-18)

1. Record the drop: a `net syn dropped` kind in `Refusal` (status `Busy`,
   the status the peer would have got if it were local), so `diag` shows
   `net syn dropped xN (code 5)`; consider answering the SYN with an RST so
   the peer stops retransmitting (RFC 793 section 3.4 allows a reset to an
   unacceptable SYN; a listener that cannot accept is one).
2. Bound the half-open connections per listener (for example 2 for a
   listener whose owner has no `TcpAccept` pending, the SYN beyond that is
   dropped and recorded) so a burst of SYNs cannot take the whole table
   from active connections.
3. A shorter retry budget for an unanswered SYN-ACK (for example 3 retries,
   about 3.5 s, as Linux's `tcp_synack_retries` default of 5 gives about
   63 s but with a table of thousands, not 8) with the loss recorded.
4. A scenario that sends `MAX_TCP` SYNs from the host without completing
   them (raw socket or a client that never ACKs), then connects from the
   guest and expects the refusal, the event and, after the budget, the
   slots back.
5. `diag` (or `net`) showing the TCP table by state, so a full table can be
   read on the machine without a capture.
