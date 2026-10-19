# ADR 0035: TCP listener protection: backlog cap, SYN|ACK budget and reset

- **Date:** 2026-10-19
- **Status:** Accepted (incident 0013; amends ADR 0024)

## Context

The network service has a TCP table of `MAX_TCP = 8` slots shared by
everything: sshd's listener, each SSH session, the shell's `http`, the
updater's downloads. Incident 0013 found two gaps in the passive-open path
(`services/net-core/src/lib.rs`, `on_tcp`):

1. A SYN to a listening port with no free slot was dropped without a word:
   no reset to the peer, no fault event. It was the only refusal of netd
   still unrecorded after incident 0012, and the peer kept retransmitting
   its SYN into the same silence.
2. Every SYN to a listening port created a `SynReceived` connection at once,
   with no bound per listener, and its SYN|ACK was retransmitted with the
   data schedule (0.5, 1, 2, 4, 8, 8, 8 s; `MAX_RETRIES = 6`), so a SYN
   whose sender never answered held a slot for about 31 s. Seven SYNs every
   30 s from an unreachable or spoofed source (a scanner on the LAN, a
   router probing port 22, clients retrying after a reboot) kept the table
   full for every active open: `Busy` for the shell, the updater and the
   owner's own SSH client. Candidate 5 of incident 0012.

The security rule ([CONTRIBUTING.md](../../CONTRIBUTING.md), "Security")
wants refusals logged; the failure protocol wants bounds that are counted
and visible in `diag`.

## Decision

1. **Backlog cap per listener.** A listener holds at most
   `tcp::listen_backlog(MAX_TCP) = MAX_TCP / 2 = 4` connections its owner
   has not accepted yet, half-open (`SynReceived`) and established alike
   (`TcpSock::accepted_from` marks them). A SYN to a listener whose backlog
   is at the cap, or that finds no free slot, creates nothing. With the
   listener's own slot that is 5 slots at most for one listening port, so a
   burst of SYNs to port 22 always leaves at least 3 slots to active opens;
   connections sshd has accepted are sshd's sessions, bounded by sshd, not
   by this cap. The cap is the listen backlog every socket API has; the
   constant and its reasoning are in `libs/net/src/tcp.rs`.
2. **The refused SYN gets a reset and is recorded.** netd answers it with
   `tcp::reset_for` (RFC 793 section 3.4: a reset for a segment of a
   connection that does not exist; sequence number 0, acknowledging the
   SYN) and accounts it as the refusal kind `net syn dropped` (incident
   0012's meter: at most one event per second, `net syn dropped x<count>`,
   code 5 = `Busy`, the status a local client gets for a full table). The
   reset tells a real client at once ("connection refused") instead of
   leaving it to retransmit for a minute, and stops its retransmissions,
   which would meet the same refusal. Towards a spoofed source the reset is
   one 40-byte segment per SYN of 40 to 60 bytes: no amplification and no
   state kept, the same reflection any closed port offers, so answering is
   acceptable here. Linux drops silently by default when an accept queue
   overflows (`tcp_abort_on_overflow = 0`) so that a client may get in on a
   retry; with 8 slots instead of thousands there is no later room to wait
   for, and a refusal the peer can see is the project's rule. A SYN to a
   port nobody listens on stays silently ignored as before: the machine
   offers no service there and a reset would only tell a scanner which
   ports are closed (closed by default).
3. **Short SYN|ACK budget.** A `SynReceived` connection retransmits its
   SYN|ACK `tcp::SYN_ACK_MAX_RETRIES = 3` times (0.5, 1, 2 s, then a last
   4 s wait: about 7.5 s) before it closes with `CloseReason::Timeout` and
   frees its slot; three retransmissions still cover a lost SYN|ACK and a
   slow LAN peer. Linux's `tcp_synack_retries` default of 5 (about 63 s)
   is scaled to the table. The schedule of active opens and of data
   (`MAX_RETRIES = 6`, about 31 s) is unchanged: their callers have
   deadlines of their own and the loss of an established connection must
   not be hurried.
4. **A closed listener does not orphan its backlog.** `TcpClose` on a
   listener aborts the connections nobody accepted (a reset on the next
   poll) and frees their slots; before, they lingered until the peer gave
   up, with no owner that could ever accept them.
5. **The table is observable.** `Op::TcpStats` (`NetServiceProtocol v1`,
   additive) returns the table by state and the SYNs reset since boot;
   `net` and therefore `diag` print one bounded line, `tcp     8 slots: 1
   listen, 0 syn-rcvd, 0 syn-sent, 3 backlog, 1 established, 0 closing, 0
   time-wait, 3 free; 4 SYNs reset`, so a full table can be read on the
   machine without a capture.

## Consequences

- One listening port can hold at most 5 of 8 slots (listener plus
  backlog). The cap is per listener: two listening ports (there is one
  today, sshd's) could together hold the whole table. A second listening
  service would need the cap shared across listeners or lowered; noted,
  not needed now.
- A legitimate SSH client arriving while four others wait unauthenticated
  is refused at once instead of queueing; sshd serves one session at a
  time with a 30 s authentication deadline, so the queue drains at that
  rate anyway and the client's retry finds room sooner than it would have
  waited.
- A half-open connection costs a slot for 7.5 s instead of 31 s: a source
  that never completes the handshake needs about 4 SYNs per 7.5 s per
  slot to keep the backlog full, and even then takes 4 slots, not 7.
- Host tests (`libs/net`): the SYN|ACK budget frees the slot at about
  7.5 s and not before; active opens and data keep the 31.5 s schedule;
  an answered SYN|ACK leaves the retry count at zero; `listen_backlog`
  leaves half the table; `reset_for` follows RFC 793 section 3.4 on the
  wire. Scenario `network.listen_backlog` (QEMU): eight plain connections
  from the host to sshd's port, held without a byte, with the shell's
  `http` succeeding in the meantime and `diag` showing the refusal and the
  table. QEMU's user networking completes the handshakes itself, so the
  scenario proves the backlog cap, the reset and the visibility with
  established connections nobody accepts, and cannot send half-open ones:
  that part rests on the host tests.
