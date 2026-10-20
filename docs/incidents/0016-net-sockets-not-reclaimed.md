# Incident 0016: netd sockets are never reclaimed when a client dies

- Opened: 2026-10-20. Status: fixed 2026-10-20 in `fix/net-client-sessions`
  (ADR 0037), reproduced in QEMU before the fix.
- Found by: the work on incident 0011 (fsd's descriptors, ADR 0036); the
  gap was already listed as open in incidents 0012 and 0013.
- Code: `services/net-core/src/lib.rs`, the TCP, UDP, ping and DNS tables
  (`tcp`, `sockets`, `pings`, `dns`) and the `Op::*` arms that fill them.

## Observation

netd has 8 TCP slots, 8 UDP sockets, 4 ping waits and 4 DNS waits for every
client together. A slot was freed only by an explicit close from a client
or by the protocol (a finished connection, an expired wait). Nothing freed
the slots of a client that died with sockets open: the tables kept them
until netd itself restarted.

Every client held a duplicate of the same main endpoint (the shell, each
SSH session shell, sshd, every program launched with `caps = net`), so
netd had no notion of whose a socket was: it could neither release a dead
client's sockets nor check that a request named a socket of its own.

## Consequence

- A program launched with `caps = net` that crashes or is killed holding
  connections leaves them in the table; with eight of them every
  `TcpConnect` of the shell, the updater and sshd's accept answers `Busy`
  (`net tcp table full`, visible since incident 0012) until reboot.
- An sshd generation that dies holding its listener keeps port 22 `InUse`
  for its successor, which exits with "cannot listen on port 22".
- Any client could operate on any socket index, including another
  client's; fsd had the owner check since its first audit, netd did not.

## Mechanism (confirmed)

The same two facts as incident 0011, in `services/net-core/src/lib.rs` at
`540a681`: the tables were keyed by index alone, with no owner, and every
client shared one endpoint, so the kernel's peer-gone report (ADR 0030)
could never apply to one client (init held the receive side of the main
endpoint and every client a `WRITE` duplicate of it).

## Reproduction

`sock_leaker` (`user/testprogs/src/bin/sock_leaker.rs`, manifest `caps =
net`): binds UDP ports until netd answers `Busy`, connects to the host echo
server until it answers `Busy`, prints `sock_leaker: holding 8 udp sockets
and 8 tcp connections; the next of each was refused (busy); exiting without
closing any` and exits. Before the fix the shell's next `http` got `network
service refused: busy` and so did every later connection until reboot.

## Fix (ADR 0037)

- Every client endpoint is a *session* netd creates with
  `endpoint_create_pair` on `Op::Attach` (16) from the main endpoint,
  keeping only the receive side and labelling it with the process name.
  init attaches one for the console shell and every SSH session shell
  (`shell`), one per sshd generation (`sshd`), and the broker one per launch
  with `caps = net` (the program's name).
- Every table is `aricode_net::owners::Owned` (`libs/net`, host-tested):
  entries record their owner, a request only reaches its own sockets
  (`BadSock` otherwise), and `reclaim` releases one owner at once.
- netd asks the kernel whether a session's client is alive with a receive
  of zero timeout (ADR 0030): once a second, whenever a table is full before
  refusing, and when a port another owner holds is asked for. A retired
  session gets its live connections reset at once, its listener freed with
  its backlog, its waits dropped; what it left open is said on the console
  and recorded as `net socks reclaimed xN` (code 0) through
  `aricode_faults`. A refused attach is `net sessions full` (code 5).
- `net` and `diag` show the tables per owner (`Op::Sockets`, 17): `sockets
  3 in use: sshd 2, shell 1; 16 reclaimed from dead clients`.
- No kernel or ABI change.

## Evidence

`network.sockets_reclaimed` (`build/scenarios/network.sockets_reclaimed/
serial.log`), first run with the fix:

```text
ari> run sock_leaker
AriInit: launch sock_leaker with manifest [net] memory 64 KiB
virtio-net: fault net udp table full x1
virtio-net: fault net tcp table full x1
sock_leaker: holding 8 udp sockets and 8 tcp connections; the next of each was refused (busy); exiting without closing any
run: sock_leaker exited with 0
ari> http http://10.0.2.2:17010/hola
virtio-net: client gone ('sock_leaker'), 16 socket(s) left open: reclaimed
virtio-net: fault net socks reclaimed x16
HTTP 200, 19 bytes
ari> diag
tcp     8 slots: 0 listen, 0 syn-rcvd, 0 syn-sent, 0 backlog, 0 established, 0 closing, 0 time-wait, 8 free; 0 SYNs reset
sockets 0 in use; 16 reclaimed from dead clients
  [3.031 s] fault reported by process 8: net udp table full x1 (code 5)
  [3.161 s] fault reported by process 8: net tcp table full x1 (code 5)
  [3.242 s] process 14 'sock_leaker' exited with code 0
  [3.435 s] fault reported by process 8: net socks reclaimed x16 (code 0)
```

The two `table full` events are the leaker's own ninth bind and ninth
connect, refused while it was alive (correct); the shell's `http` found the
TCP table full, netd swept, reset the eight connections and served it. The
reclaim happened at the shell's request, about 190 ms after the leaker
exited, not at the periodic sweep.

## Tests

- Host: `libs/net` (`owners` module: owner isolation, full table, reclaim
  of one owner, disowned entries), `sdk/proto` (`Attach`, `Sockets`,
  labels, `OwnerSockets` packing).
- QEMU: `network.sockets_reclaimed` (new); `network.*`, `shell.*`, `ssh.*`,
  `update.*`, `diag.*`, `apps.*` green with the change (sessions for the
  shells, sshd and every `caps = net` launch).

## Audit follow-ups (2026-10-20)

Two independent audits of ADR 0037 found nothing serious; the low items
were fixed on `fix/netd-audit-followups` (details in ADR 0037, "Audit
follow-ups"):

- `run` rebuilds the `ipc_receive_any` endpoint list inside a round when a
  request granted or retired a session (a retired session's closed handle
  used to end the round with `BadHandle`; a new session waited a round).
- Without a network card the shell no longer gets a duplicate of init's
  undrained main endpoint: its slot holds a placeholder that reaches nobody
  and `net`, `ping`, `dns` and `http` print `no network service`
  (scenario `shell.no_network`, scenario option `no_nic`).
- `network.listen_backlog` asserts the socket line; the ADR said `sockets
  5`, the serial log showed `sockets 6 in use: sshd 6` (listener, session,
  4 backlog; 5 is possible when sshd had not accepted yet).
- `net session create` / `fs session create` for a failed
  `endpoint_create_pair`, apart from `sessions full`.
- ADR 0037 records that a retired SYN-SENT connection's RST|ACK (ack 0)
  is harmless and that these resets use the ARP-deferred path, unlike
  incident 0013's stateless ones.
- `Request::decode` rejects a label that is not valid UTF-8.
- A shell whose netd session died with a netd restart (`ServiceGone`) says
  that the network service was restarted and the shell must be restarted.

## Open

- Sockets opened on the main endpoint (init's health check, the boot test
  clients) are released only by close; they run to completion under init's
  watch and close what they open (ADR 0037 §6).
- A session dies with the netd generation that created it; a client of a
  relaunched netd sees `ServiceGone` instead of reaching the new generation
  (ADR 0037, consequences); the shell now says so and asks to be
  restarted. No scenario restarts netd today.
- The event name is `net socks reclaimed`: `sockets` would not leave room
  for the count in the kernel's 24-byte event name.
