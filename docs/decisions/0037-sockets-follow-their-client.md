# ADR 0037: Sockets follow their client

- **Date:** 2026-10-20
- **Status:** Accepted (incident 0016; amends ADR 0024 and ADR 0035,
  applies the design of ADR 0036 to the network service, uses ADR 0030)

## Context

netd keeps four small tables for every client together: 8 TCP slots, 8
UDP sockets, 4 ping waits and 4 DNS waits. An entry was freed only by an
explicit close from a client or by the protocol itself (a connection that
finished, a wait that expired). Nothing freed the entries of a client that
exited without closing: a program launched with `caps = net` that crashed
or was killed, a shell command interrupted by a fault, an sshd generation
that died holding its listener. The gap was noted in incidents 0012 and
0013 as the socket twin of incident 0011 (fsd's descriptors) and is
incident 0016. Its worst case is fsd's: eight leaked connections and every
`TcpConnect` answers `Busy` (`net tcp table full`) until reboot; a dead
sshd's listener keeps port 22 `InUse` for its successor.

Ownership was also missing on the request path. Every client held a
duplicate of the same main endpoint, so netd could not tell one client's
request from another's: a socket index named in a request was honoured
whoever sent it. fsd had the owner check since its first audit; netd did
not.

## Options considered

(a) **Per-client sessions**, as ADR 0036 did for fsd: init asks netd for a
fresh endpoint for every process it or the broker launches, netd keys
every table by the endpoint a request arrived on and, when the kernel
reports the client gone (ADR 0030), closes everything it held. Exact, no
guessing, no new kernel interface.

(b) **Leases on idle sockets.** Rejected for the reason ADR 0036 gives:
a live idle socket (sshd's listener between connections, a shell's `http`
waiting on a slow server) is indistinguishable from a leaked one, and a
reclaimed live socket is a silent error.

**Decision: (a).** The mechanism is the one fsd has, so the two services
now share one model and one vocabulary (`Attach`, sessions, sweep, retire,
`reclaimed` events, a table per owner in `diag`).

## Decision

1. **Sessions.** Every endpoint netd serves besides its main one is a
   *session* created by `Op::Attach` (16) from the main endpoint, which
   only init holds. netd creates it with `endpoint_create_pair`, keeps the
   receive side and transfers the send side in the reply. The request
   carries a label (`Request::label`, up to `MAX_LABEL = 24` bytes, the
   kernel's bound for process names): the process name init launches, so
   console lines and `diag` can name the client. One session per launch,
   never shared.
2. **Owner = session.** Every table is `aricode_net::owners::Owned<T, N>`
   (`libs/net`, host-tested): each entry records the owner that created it
   (0 = main endpoint, `slot + 1` = session). A request names an index and
   is served only if its owner holds that index, otherwise `BadSock`:
   `TcpAccept`, `TcpSend`, `TcpRecv`, `TcpClose`, `UdpSend`, `UdpRecv`,
   `UdpClose`. A passive connection created by a SYN belongs to the
   listener's owner, who is the only one that can accept it.
3. **The kernel says when a client is gone.** Nobody but the client (and
   whoever it transferred its handle to) holds a session's send side, so
   when the process is reaped the side closes (ADR 0030, items 2 and 4). A
   receive with a zero timeout on the session endpoint answers
   `PeerClosed` once that happened and the queue is empty, `Timeout` while
   the client lives. netd asks (a *sweep*) once a second from its loop and
   on demand before refusing: a `TcpConnect`/`TcpListen`, `UdpBind`,
   `Ping`, `Dns` or `Attach` that finds its table full sweeps first; a
   `TcpListen` or `UdpBind` that finds the port held by another owner
   probes that owner alone before answering `InUse` (sshd relaunched before
   the sweep noticed its previous generation). A request a probe takes from
   a live session is kept and served before the next wait. If the sweep
   finds the requesting client itself dead, its request ends there (the
   reply endpoint is closed and nothing is created for an owner number the
   next client would inherit).
4. **Retiring a session** closes netd's handle and everything the owner
   held: a live connection is aborted and its reset sent at once, so the
   slot is free for the request that found the table full; a listener goes
   with its backlog (same owner); a pending connect, accept, receive, ping
   or DNS wait releases its handles (nobody will read the reply). A
   connection the client had closed before dying (TIME-WAIT, LAST-ACK) is
   left to finish by itself and handed to `NOBODY`, an owner no session
   has, so it counts for no one and is never reclaimed again. A session
   retired with sockets open is a fault of its client: `virtio-net: client
   gone ('sock_leaker'), 16 socket(s) left open: reclaimed` on the console
   and `net socks reclaimed xN` (code 0) in the kernel event ring through
   the shared `aricode_faults` meter (`report_many`: one dead client with
   sixteen sockets is one event). "socks" because `net sockets reclaimed`
   plus ` x999` does not fit the 24-byte event name, which is checked at
   compile time for every kind. A refused `Attach` is `net sessions full`
   (code 5) when the table is full and `net session create` (code 5) when
   the kernel could not create the endpoint pair.
5. **Who gets a session.** init's `spawn_shell` attaches one labelled
   `shell` for the console shell and every SSH session shell; `start_sshd`
   one labelled `sshd` per generation; the broker one labelled with the
   program name for every launch with `caps = net` (applications, the
   updater, `tcp_hog`, `sock_leaker`). A refused attach is said on the
   console (`AriInit: network session refused by netd (status N)`) and the
   launch fails rather than falling back to the shared endpoint.
6. **Who stays on the main endpoint and why.** init itself (its read-only
   `Info` health check; init never exits) and the boot test clients
   `net_client`, `tcp_client` and `http_client`: they run only with
   `selftest` before the shell exists, init waits for their exit, and they
   close what they open (their scenarios assert the success lines). A leak
   there would be a test-program bug and would show in `diag` as sockets
   held by `main`. Sockets on the main endpoint are released only by close.
7. **Session capacity.** `MAX_SESSIONS = IPC_MAX_ANY - 1 = 15`, served
   together with the main endpoint by one `ipc_receive_any`. The clients
   that can hold one at the same time are the console shell, up to
   `MAX_SESSIONS = 4` SSH shells, sshd and the broker's `MAX_RUNNING = 4`
   programs: ten, so unlike fsd (one `Bind` plus one `Share` per file per
   launch, ADR 0036 §6) the table is never full for a legitimate load; a
   full table is still swept and, if still full, refused visibly.
8. **`diag` shows the tables** (`Op::Sockets`, 17, any client): one line
   after the TCP table, `sockets 3 in use: sshd 2, shell 1; 16 reclaimed
   from dead clients`, one entry per owner holding something (`main` for
   init, otherwise the session label), counting TCP, UDP, ping and DNS
   together; the TCP line keeps the states. The counts travel packed in
   `Reply::value` (`net::OwnerSockets`, host-tested), the label in
   `Reply::data` with its length in `Reply::port`.

## Handle changes

- The shell's handle `0x10009`, sshd's `0x10004` and the `net` capability
  handle of a launched program are now the send side of a session (`WRITE
  | DUPLICATE | TRANSFER`) instead of a `WRITE` duplicate of the main
  endpoint. Their position and rights for the client are unchanged; no
  client code changed.
- init keeps the main endpoint (all rights) for `Attach`, its health check
  and the boot test clients; the driver keeps its `READ | WRITE` handle.
- `Request` gains `label`/`label_len` in bytes 20..45 (unused before);
  `Reply` gains `with_label`/`label_str` over the existing `data` and
  `port` fields. `Op` 16 and 17 are additive; `VERSION` stays 1.

## Privilege analysis

- A session grants exactly what the shared endpoint granted minus
  `Attach`, which only the main endpoint may use: no client can create
  sessions.
- A request can only reach sockets its own session created (or was handed
  by its own listener). The owner number of a retired session is reused
  only after everything it held was released or disowned, so a new client
  never inherits an index. Guessing an index still gets `BadSock`.
- Two launches of one application no longer share an endpoint, so one
  instance cannot operate on the other's connections through shared
  indices (possible before through the common endpoint).
- `Op::Sockets` tells any client which owners exist, their labels and how
  many sockets each holds: the same information `ps` and the TCP line
  already give, no addresses or ports.
- A process that duplicates and transfers its session handle extends its
  sockets' lifetime to the receiver, which is the capability model working
  as intended.

## Rejected alternatives

- An `ipc_receive_any` variant reporting which endpoint closed: more ABI
  for what a zero-timeout receive already answers (ADR 0036).
- Leases (option b).
- Reclaiming TIME-WAIT connections of a dead client with a reset: frees a
  slot two seconds sooner at the cost of TCP's protection against old
  duplicates, for a client that did close properly. (Incident 0027 later
  recycles a TIME-WAIT slot without a reset, but only when the table is
  full and a new connection needs it; the new connection does not get the
  old pair again until the port counter wraps.)

## Consequences

- `network.sockets_reclaimed`: `run sock_leaker` (manifest `caps = net`,
  stripped from a stick like `tcp_hog`'s) fills both tables and exits
  without closing; the shell's `http` finds the TCP table full, netd sweeps
  and the connection goes through (`HTTP 200`); `diag` shows `sockets 0 in
  use; 16 reclaimed from dead clients`, `net udp table full x1`, `net tcp
  table full x1` (the leaker's own ninth of each) and `net socks reclaimed
  x16`. The reclaim happened 190 ms after the leaker exited, at the shell's
  request, not at the periodic sweep.
- `network.listen_backlog` shows every socket on sshd next to the TCP
  line: `sockets 6 in use: sshd 6; 0 reclaimed from dead clients` in the
  run checked (listener, its session, 4 backlog), or 5 when sshd had not
  accepted its session before the backlog cap was reached; the scenario
  asserts ` in use: sshd ` and nothing reclaimed, and that the count never
  reaches 7.
- A session dies with the netd generation that created it: if the
  supervisor relaunches netd, its clients get `ServiceGone` on their
  session instead of reaching the new generation through the shared main
  endpoint. sshd already exits on `ServiceGone` and is relaunched with a
  new session; a shell must be restarted, and says so: a network command
  that gets `ServiceGone` (the send refused, which ADR 0030 tells apart
  from `ReplyLost`, a request dropped unanswered) prints `network service
  was restarted: this shell's session ended; restart the shell ('exit')`
  instead of the generic `network service not responding` (`exit` on the
  console makes init launch a new shell with a new session). fsd's
  sessions behave the same (ADR 0036); no scenario restarts netd today,
  and the change is recorded here so it is not a surprise when one does.
- The sweep costs one syscall per live session per second.
- Not covered: sockets opened on the main endpoint (init, the boot test
  clients) are released only by close.

## Audit follow-ups (2026-10-20)

Two independent audits of the merged change found nothing serious; the
low items were fixed on `fix/netd-audit-followups`:

- **Stale endpoint list.** `run` built the list for `ipc_receive_any`
  once per round of eight requests. A request that retired a session (a
  sweep or probe inside `Attach`, `TcpConnect`, ...) left its closed
  receive handle in the list, the next receive answered `BadHandle` and
  the rest of the round was skipped; a session granted mid-round waited for
  the next round. netd now counts every grant and retire (`generation`)
  and rebuilds the list before the next receive when it changed.
- **No network service.** Without a network card `spawn_shell` gave the
  shell a `WRITE` duplicate of the main endpoint nobody serves, so every
  `net`, `ping`, `dns` or `http` queued a request with its reply endpoint
  in a queue init never drains. The shell's handle slots are positional
  and the kernel does not take `Handle(0)` in a spawn list, so the slot now
  holds the send side of a pair whose receive side init closes at once (it
  reaches nobody: a send fails with `PeerClosed`), and the shell, seeing
  `SERVICE_NET` off, answers those commands with `no network service`
  without sending anything; `diag` keeps leaving the network section out.
  Scenario `shell.no_network` (new scenario option `no_nic`).
- **The listen_backlog claim** above was corrected from the serial log
  (6, not 5) and is now asserted.
- **`net session create`**: an `endpoint_create_pair` failure in `Attach`
  is no longer reported as `net sessions full`; fsd got the same split
  (`fs session create`).
- **Resets of retired connections.** Retiring a session aborts its live
  connections and sends their reset at once. For a connection still in
  SYN-SENT that is an RST|ACK with acknowledgment number 0 to a peer that
  never answered: the peer has no matching connection and ignores it
  (RFC 9293 §3.10.7.1: a reset for no connection is discarded), so it is
  harmless and not worth a special case.
  These resets go through the normal output path, which parks a frame
  whose next hop is not in the ARP cache until the ARP reply: by design,
  since they belong to connections netd held state for and the peer was
  just talking to us. Incident 0013's stateless resets to SYNs beyond a
  listener's cap are the opposite case (no state, possibly spoofed
  sources) and are never parked.
- **Label validation.** `Request::decode` rejects a label that is not
  valid UTF-8 (`Status::Invalid`, `net bad request`) instead of reading it
  as an empty name; proto host test.
- **netd restart** is now said by the shell (Consequences, above).
