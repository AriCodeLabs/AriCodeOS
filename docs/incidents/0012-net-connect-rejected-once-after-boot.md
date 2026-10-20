# Incident 0012: `http` refused once by the network service after a boot

- Opened: 2026-10-18. Status: root cause open; visibility fixed (below).
- Hardware: the reference laptop (`docs/hardware.md`), Realtek `10ec:8136`
  through `rtl_net` over `ari-net-core`, one SSH session from the host.
- Software: version 7 on the freshly upgraded slot = `develop` `e8f570b`,
  about 80 s after the reboot into that slot.

## Observation

Over SSH, right after `update`, `cat updater/state`, `write`, `cat`, `ls`
and `services` had run, the command

```text
ari> http https://github.com/
http: could not connect: network service rejected the request
```

A few minutes later the same command answered `HTTP 200`. Eight quick SSH
connections in a row followed by an HTTPS fetch did not reproduce it.
`diag` recorded no fault event at all: the kernel ring held only the
process events of the session.

## What the evidence can and cannot tell

The message comes from `net_error` in `services/shell/src/main.rs`. Before
this incident it printed three statuses by name (`Timeout`, `NoRoute`,
`NotFound`) and lumped every other `NetError::Status` into "network service
rejected the request". The prefix `http: could not connect:` places the
failure in the `Connect` phase of `aricode_https::get`: the DNS resolution
of `github.com` had already succeeded (a resolution failure is printed
without that prefix). So netd answered `TcpConnect` with one of:

| Status       | Where netd produces it for `TcpConnect`                                   |
|--------------|---------------------------------------------------------------------------|
| `Busy`       | no free slot in the TCP table (`MAX_TCP = 8`, `tcp_handle`)               |
| `Reset`      | the SYN was answered with RST, or the connection was aborted (`tcp_poll`) |
| `Invalid`    | not produced for `TcpConnect` (only for an undecodable request)           |
| `BadHandles` | not produced for `TcpConnect` (no client buffer travels with it)          |

The message cannot tell `Busy` from `Reset`, and nothing else recorded the
refusal: netd answered the status and forgot it. The fact that `diag`
showed nothing is therefore not evidence that nothing went wrong in netd;
netd simply never recorded refusals. The absence of a fault event rules
out only a crash or restart of `rtl_net` (init would have recorded it).

The timing is known loosely: the refusal came within the first minutes
after boot, after an `update` run (which opens connections of its own to
every source) and with one SSH session established; minutes later it did
not recur, and a burst of eight SSH connections plus one HTTPS fetch did
not reproduce it either.

## Candidate causes

1. **TCP slot exhaustion** (`Busy`). The table has eight slots shared by
   everything: sshd's listener takes one for good, each SSH session one
   more, and every connection the updater or the shell closes actively
   stays in its slot for the 2 s TIME-WAIT (`libs/net/src/tcp.rs`,
   `TIME_WAIT_MS`) before `tcp_poll` frees it. A `TcpConnect` arriving
   while eight slots are listening, established, closing or in TIME-WAIT is
   refused. Against it: the shell commands between `update` and `http`
   took longer than 2 s, so the updater's connections should have left the
   table; unless the updater or sshd left connections that never finish
   (a peer that never answers the FIN keeps a slot until the
   retransmissions give up, up to tens of seconds), or a client died with
   sockets open, which netd never reclaims (the same gap arifs has for
   descriptors, incident 0011).
2. **ARP deferral on the first contact with the gateway** (would surface
   as `Timeout`, not as this message). The first frame to a next hop whose
   MAC is unknown is parked for up to 1 s (`ARP_WAIT_MS`) and dropped
   silently if the reply does not come; TCP retransmits the SYN from
   `snd_una`, so a lost first SYN costs an RTO, not a refusal. A full
   parking table (`deferred`, 4 slots) does answer `Busy`, but only to the
   request that sends the frame itself (`Ping`, `UdpSend`, `Dns`): TCP
   segments are sent from `tcp_poll`, which ignores the error. This
   candidate cannot produce the observed message; it is kept because it
   was silent too, and the new events make it observable.
3. **Frame send failure** (`send_frame` returning `false`): the Realtek
   transmit ring has 8 descriptors; a full ring loses the frame. As with
   2, a `TcpConnect` never sees this as a status, only as a slower
   connect or a timeout. Silent before this incident.
4. **RST from the other end or the NAT** (`Reset`). After every reboot
   netd restarts its ephemeral ports at `TCP_EPHEMERAL_FIRST = 50000` and
   its initial sequence number is `now * 64 + slot` (small, predictable
   shortly after boot). A SYN to `github.com:443` from a 4-tuple that the
   previous boot used minutes earlier, with an ISN below the one the server
   remembers, can be answered with an RST or a challenge ACK by a server
   still holding that connection (RFC 793 section 3.4; RFC 5961 section
   4); the home router's NAT may also still map the old flow. `tcp_poll`
   turns the RST into `Status::Reset`, which the shell printed as
   "rejected the request". Consistent with "once, right after boot, never
   again": the next attempt uses another port. Not provable from the
   evidence kept. Incident 0014 (`0014-predictable-isn-and-ephemeral-
   ports.md`) treats the predictability itself as a security defect.
5. **Half-open connections holding the table** (`Busy`, found by the audit
   of the fix). Every unsolicited SYN to a listening port (22 on the
   laptop, the SSH listener) creates a `SynReceived` socket that keeps its
   slot while the SYN-ACK is retransmitted (about 31 s: RTO 0.5, 1, 2, 4,
   8, 8, 8 s), and a SYN that finds no free slot is dropped silently, the
   one refusal still unrecorded. Seven stray SYNs every 30 s (a scanner on
   the LAN, the router probing, a client retrying) keep the 8-slot table
   full for everyone, and the shell's `TcpConnect` gets `Busy`. The
   laptop sits behind a home router, so unsolicited SYNs from the Internet
   are unlikely, but SSH clients on the LAN retrying after the reboot are
   not. Incident 0013 (`0013-syn-drop-silent-half-open-hold.md`).

## Plan

1. Make every refusal visible (this branch, `fix/net-refusal-visibility`):
   the shell names the status, netd records a fault event with a bounded
   rate, and a scenario pins both behaviours.
2. Wait for a recurrence on the laptop with the new diagnostics: the shell
   line distinguishes `Busy` ("refused: busy ...") from `Reset`
   ("connection refused or reset by the other end"), and `diag` shows
   whether netd refused anything, what, and how many times.
3. Depending on what recurs: for 1, reclaim the sockets of dead clients
   (ADR 0030 `PeerClosed`, shared with incident 0011) and count slots per
   state in `diag`; for 4, randomise the ephemeral port and the ISN (RFC
   6528) at boot with the kernel's entropy (`crypto.random`).

## Fix (visibility only)

The root cause stays open until the refusal recurs with these diagnostics.

- `NetError::describe` (`sdk/rust/src/net.rs`, host-tested): one line per
  status. Refusals by netd read `network service refused: busy (no free
  connection or send slot)`, `... invalid (malformed request)`, `... bad
  handles (...)`, `... bad socket (...)`, `... port in use`; protocol
  outcomes keep their own wording (`no reply (timed out)`, `no network
  configuration`, `unknown name`, `connection refused or reset by the
  other end`). The shell's `net_error` prints it; "rejected the request"
  no longer exists.
- netd (`services/net-core`) records a fault event (`sys::event_record`,
  ADR 0028) whenever it refuses a request or loses a frame, named by cause:
  `net tcp table full`, `net arp defer full`, `net tx busy`, `net arp no
  reply`, `net ping table full`, `net dns table full`, `net udp table
  full`, `net wait pending`, `net bad request`, `net bad handles`. The
  rate is bounded by `aricode_net::faults::Meter` (host-tested): at most
  one event per kind per second; the refusals held back in between are
  counted and recorded when their second ends (`drain_due`), so a burst
  that stops still shows its size. The event name is `<kind> x<count>`
  (count in that second, up to 999; the longest kind plus ` x999` fills
  the 24-byte name, checked at compile time) and the code is the
  `net::Status` number the client got (5 busy, 1 invalid, 4 bad handles),
  since `diag` prints every producer's code as a reason number: `fault
  reported by process N: net tcp table full x3 (code 5)` = three refusals
  answered Busy. The same text goes to the console (`virtio-net: fault net
  tcp table full x3`). `BadSock`, `InUse`, `NoRoute`, `NotFound`,
  `Timeout` and `Reset` are not refusals and are not recorded.
- Scenario `network.refusal_visible`: `run tcp_hog` (new test program with
  the manifest `caps = net`, `user/testprogs/manifests/`) opens
  connections to the host echo server until netd refuses the ninth, prints
  the status as the shell would, and closes them; the shell's `http` at the
  next prompt finds the eight slots in TIME-WAIT and prints the new line;
  `diag` lists the event.
- Documented in `docs/architecture.md` (TCP section) and ADR 0024.

Evidence from the scenario (QEMU, `build/scenarios/network.refusal_visible/
serial.log`):

```text
virtio-net: fault net tcp table full x1
tcp_hog: 8 connections open; the next one: network service refused: busy (no free connection or send slot)
run: tcp_hog exited with 0
http: could not connect: network service refused: busy (no free connection or send slot)
  [2.749 s] process 14 'tcp_hog' created
  [2.881 s] fault reported by process 8: net tcp table full (code 1)
  [2.973 s] process 14 'tcp_hog' exited with code 0
```

The shell's refusal came within the same second as the hog's, so the meter
held it back and records it as `net tcp table full x1` when that second
ends (with the first version of the fix it would have waited for the next
refusal; the audit changed that); the eight slots were indeed still taken
by the hog's closed connections in TIME-WAIT. The event line above shows
the first version's format (`(code 1)` = count); the final one is `net tcp
table full x1 (code 5)`.

First yield of the new events, across the 58 scenarios of `--area
network,shell`: every boot with `selftest` now shows one `virtio-net: fault
net arp no reply x1` about 1.8 s after boot. The capture (`net.pcap`,
scenario `network.stack`) shows why: `net_client` pings `10.0.2.99`, an
address nobody owns, to test the ping timeout; the ARP request at 0.773 s
gets no reply and the parked echo is dropped at 1.773 s. Expected and
previously silent; `diag` lists it as `fault reported by process N: net arp
no reply (code 1)`. No `net tx busy` or `net arp defer full` appeared in
any scenario, HTTPS and update downloads included.

## Open

- The one-off itself: `Busy` or `Reset` is unknown. Next occurrence tells.
- netd did not reclaim the sockets of a client that exits without
  closing them (as arifs with descriptors, incident 0011): incident 0016,
  fixed by ADR 0037 (per-client sessions; `net socks reclaimed` events and
  the `sockets` line of `diag`). Candidate 1 can now be read on the machine.
- Ephemeral ports and ISNs restarted from fixed values at every boot:
  incident 0014 (security), fixed by ADR 0034 (per-boot secret from the
  kernel's entropy, RFC 6528 ISNs, RFC 6056 ports). Candidate 4 goes from
  certain to improbable (about 1/16384 per connection towards a server
  still holding one old connection, with the new ISN lower about one time
  in two), not to impossible: a recurrence may still read `Reset`; `Busy`
  points at candidates 1 or 5.
- A SYN to a listener with no free slot is dropped silently and half-open
  connections hold their slot for about 31 s: incident 0013.
- `tcp_poll` ignores send failures of TCP segments: the new `net tx busy`
  and `net arp defer full` events make them visible, but the segment is
  still only recovered by retransmission.
- `run tcp_hog` on a stick launches without a manifest and does nothing
  (ADR 0032 §5); the scenario initrds keep the manifest.
