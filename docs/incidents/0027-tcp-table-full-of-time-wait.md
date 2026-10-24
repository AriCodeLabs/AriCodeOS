# Incident 0027: a TCP table full of TIME-WAIT refused the next download

- Opened: 2026-10-24. Status: fixed on `fix/tcp-time-wait-recycling`.
- Found by: the first test of the public source's local mirror (ADR
  0048) on the reference laptop: right after four quick SSH commands,
  `update` through `.../releases/latest/download/` fetched `index` (three
  connections, two redirects) and failed before asking for `index.sig`.
  The same `update` seconds later worked.
- Environment: netd with `MAX_TCP` = 8 slots and `TIME_WAIT_MS` = 2 s;
  any burst of connections the machine closes first.

## Evidence

The laptop's event ring at the failure:

```text
[56935.914 s] fault reported by process 6: net tcp table full x1 (code 5)
[56935.958 s] fault reported by process 24: upd-index-fetch (code 30)
[56935.987 s] fault reported by process 24: upd-no-verified-index (code 35)
```

Reproduced in QEMU by `network.time_wait_recycled` once the test
server's mirror waits for the client to close first, as a server across a
real network does: four downloads through the mirror, twelve connections
in a few seconds.

```text
tcp     8 slots: 0 listen, 0 syn-rcvd, 0 syn-sent, 0 backlog, 0 established, 0 closing, 8 time-wait, 0 free; 0 SYNs reset
virtio-net: fault net tcp table full x1
http: could not connect: network service refused: busy (no free connection or send slot)
```

`update.redirected_source` (nine connections) failed the same way under
that server.

## Mechanism

The side that closes a TCP connection first keeps it in TIME-WAIT for
`TIME_WAIT_MS`, and netd keeps its slot until then. The HTTP client closes
as soon as it has the whole body, and sshd closes a session when it ends,
so each of those holds a slot for two seconds after it is over. Eight
slots, one listener and one live session leave six; a download through
the public source takes three connections per file since ADR 0048
followed redirects, and four SSH commands just before took four more. The
table was full of finished connections, and `tcp_room` refused after its
sweep, which only looks for dead clients.

## Fix

- When the table is still full after the sweep, netd gives the new
  connection the slot of the closed connection in TIME-WAIT that would end
  soonest (`tcp::recyclable`, host-tested; only connections their client
  has closed), and records it (`net tcp tw recycled`, code 0). Both
  outgoing connections and incoming SYNs (within the listener's backlog)
  use it. TIME-WAIT guards that old connection's own address and port
  pair, which the new connection does not share (a fresh ephemeral port
  or another peer), and every initial sequence number comes from the
  per-boot secret (ADR 0034); lwIP does the same (`tcp_kill_timewait`).
- `net tcp table full` now means every slot holds a live connection.
- Tests: `network.time_wait_recycled` (the reproduction above: every
  download completes and the recycling is recorded) and
  `update.redirected_source` under the same server; host test of the
  choice.

## Open

- Eight slots are few for a machine that serves SSH and downloads at
  once; their size (a boxed connection each, in a 256 KiB heap) is the
  limit. Recycling covers bursts of finished connections, not more live
  ones.
