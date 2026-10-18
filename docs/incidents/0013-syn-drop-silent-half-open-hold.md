# Incident 0013: silent SYN drop and half-open connections holding the TCP table

- Opened: 2026-10-18. Status: open, found by reading (not reproduced).
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

## Plan

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
