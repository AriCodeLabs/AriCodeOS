# ADR 0030: Peer-gone notification on IPC endpoints (`PeerClosed`)

- **Date:** 2026-10-17
- **Status:** Accepted

## Context

ADR 0012 bounded every IPC wait, and incident 0001 made the waits long on
purpose (block client 90 s, file client 120 s) so that a slow USB stick is
latency and never a client that gives up while the server still holds its
buffer. The price, recorded as a known limitation in incident 0001: a
client of a server that is *gone* also waits the whole time. The scenario
`filesystem.corrupt_detected` (fsd refuses a corrupt AriFS and exits)
needed 200 s, and sshd held every SSH banner behind a reread of
`authorized_keys` from a dead file service. The kernel had no way to tell
a client "nobody will answer".

## Who "the server" is in this capability model

An endpoint is one kernel object; what a holder can do with it is set by
the rights of its handle. There is no owner and no fixed client/server
pair, so the kernel cannot know "the server process". What it can know is:

- the **receive side** of an endpoint: every capability with `READ` on it;
- the **send side**: every capability with `WRITE` on it.

Capabilities live in three places: handle tables, messages in flight
(transferred handles are removed from the sender before the message is
queued) and the handle lists the kernel builds for a new process. They
move between those places without being copied; only `handle_duplicate`
and object creation make new ones. So both sides can be counted exactly.

For a request/reply call (ADR 0012) this gives:

- the service endpoint's receive side is the server process **and**, for a
  supervised service, init: the supervisor keeps the receive side so that
  the next generation can serve the same endpoint (ADR 0011);
- the reply endpoint's send side is the single `WRITE` capability that
  travels with the request; while the request is queued it lives in the
  message, once received it lives in the server's handle table. When the
  server dies holding it, or the queue holding it is discarded, it is gone.

## Decision

1. **Counted capabilities.** The kernel's handle tables hold `object::Cap`
   (object plus the endpoint sides its rights open) instead of a bare
   reference. Creating a `Cap` with `READ`/`WRITE` on an endpoint counts it
   on that side; dropping it uncounts it. `HandleTable::duplicate` builds
   the new entry from the new rights, so a reduced duplicate counts only
   the sides it keeps.
2. **Sides close once.** When the last capability of a side goes, the side
   closes for good (no capability for it can be made again: duplicating
   needs one). Receive side closed: the queue is discarded (its
   capabilities are dropped, which can close the reply endpoints inside),
   and every waiting and future `ipc_send` fails with the new
   `Error::PeerClosed` (ABI code 13). Send side closed: `ipc_receive`
   delivers what is queued, then fails with `PeerClosed` instead of
   waiting. `ipc_receive_any` reports `PeerClosed` only when every one of
   its endpoints is empty and closed (one closed endpoint among live ones
   simply stays empty).
3. **No work under locks.** A capability can be dropped with a process
   table or an endpoint queue locked, so dropping one only updates atomic
   counters and, when a side closes, queues the endpoint
   (`ipc::settle_later`, a leaf lock). `ipc::settle_closed` discards the
   queues and wakes the waiters at points with no lock held: the end of
   every syscall and after the scheduler reaps finished threads. It works
   iteratively, so a chain of endpoints closed by discarded queues does
   not recurse.
4. **Process exit releases capabilities at reap.** A process's handle table
   used to be dropped only when a parent removed it from the process table
   with `process_wait`. It is now released when the process is reaped
   (right after its address space and IOMMU domain), so a dead server's
   receive side and the reply capabilities it held close at once, whether
   or not anybody waits for it. They are released and settled before the
   process is marked reaped.
5. **`endpoint_create_pair` (syscall 34)** returns a receive handle
   (`READ|DUPLICATE|TRANSFER|MANAGE`) and a send handle
   (`WRITE|DUPLICATE|TRANSFER`) in one call. `rpc::call` uses it: the
   client keeps only the receive side and sends the other with the
   request, one syscall fewer than create plus duplicate. Clients that
   keep `WRITE` on their reply endpoint (init's probes) behave as before.
6. **Closing on purpose.** `object_revoke` on an endpoint now also closes
   both sides (a revoked endpoint used to leave its waiters asleep until
   their timeouts). New `endpoint_close` (syscall 35, `MANAGE`) closes
   both sides without invalidating the handles. init's supervisor calls it
   on every endpoint a service served (its endpoint and, for a USB
   controller, the block endpoint) when it gives the service up (budget
   exhausted or `strict`): it is the holder of the receive side that knows
   nobody will serve it again. It does not when the process could not be
   killed (it may still be serving). While a restart is pending, queued
   requests wait for the next generation, as ADR 0012 promised.
7. **ABI version 4**: new error code, two new syscalls, and the changed
   meaning of sends to an endpoint nobody can receive from (they used to
   succeed and the message was lost; a late reply now gets `PeerClosed`).
   Most servers ignore the result of a reply send; usb-input reports it
   (`block reply undelivered`, a fault event), and now does so whenever a
   client gave up before the reply, as its comment always intended. A
   handle listed twice in `ipc_send` or `process_spawn` is `Invalid`.

## Consequences

- A request a dead server held, or that sat in the queue of a server whose
  endpoint closed, ends at once with `PeerClosed`; only an alive and slow
  server costs the full reply wait. The long waits of incident 0001 stay.
- The SDK clients pass the error through their types (`FsError::Ipc`,
  `NetError::Ipc`, the block client's `Error`); `FsError::service_unavailable`
  groups `ServiceGone`, `ReplyLost` and `Timeout` for callers that only need
  "no answer" (sshd, item 2 of the incident follow-ups).
- A shell in an SSH session gets `PeerClosed` and ends when sshd dies (sshd
  held the only `WRITE` on the session input).
- `filesystem.corrupt_detected`: init supervises fsd while its test client
  runs; after the restart budget it closes fsd's endpoint and the client
  fails at once; the scenario timeout is back to 60 s.
- `test=ipc_peer` (`ipc.peer_closed`, programs `peer_server` and
  `peer_client`) covers the server dying with the request held, queued and
  with a sender blocked, plus the endpoint-local cases, each bounded to
  5 s against a 30 s client wait.
- A closed side never reopens: an endpoint closed by `endpoint_close` or by
  losing its receive side cannot be served again; a supervisor that wants
  to serve again creates a new endpoint.
- **Only the send side is permanent.** A send refused with `PeerClosed`
  or `Revoked` means nobody can receive on that endpoint again; a receive
  on one's own endpoint that reports them (`Error::endpoint_gone`) likewise.
  A reply endpoint closing is different: the server dropped *this* request
  (it died holding it), but init keeps the receive side of a supervised
  endpoint and relaunches the server on it, so the next request may be
  served. `rpc::call` therefore tells them apart with two SDK-level codes
  that no syscall returns: `Error::ServiceGone` (send refused: stop using
  the endpoint) and `Error::ReplyLost` (reply side closed: retry after a
  short pause; if the service was given up, the retry gets
  `ServiceGone`). They travel through the client error types
  (`InputError::Ipc`, with `service_gone()`/`reply_lost()`; `NetError::Ipc`;
  `FsError::Ipc`; the block client's `Error`). A client must stop retrying
  only after `ServiceGone`: a polling loop that treats it like a timeout
  spins. console-bridge and the compositor drop the input device on
  `ServiceGone` (logged once) and read again after 50 ms on `ReplyLost`;
  console-bridge parks, instead of exiting, when every device is gone
  (exiting would only make init relaunch it to find the same). sshd exits
  on `ServiceGone` from the network service and listens again after a
  `ReplyLost` or a socket the relaunched driver no longer knows. fsd,
  virtio-blk, virtio-input, net-core and usb-input exit when their own
  service endpoints report `endpoint_gone`. `Timeout` keeps meaning "try
  again later". Scenario `services.input_restart` (`input_restart`): an
  input driver crashes holding a read; the client gets `ReplyLost` and the
  relaunched device delivers events again.
- `ipc_send` also returns `Revoked` when one of the transferred handles is
  revoked, not only the endpoint. `rpc::call` maps a `Revoked` send to
  `ServiceGone` only after checking the service endpoint itself
  (`object_kind` on it reports `Revoked`); otherwise the caller gets
  `Revoked` as is. The check costs a syscall on that error path only.
- A shell in an SSH session: sshd creates the session endpoints with
  `endpoint_create_pair` and keeps only the send side of the input and the
  receive side of the output, so a shell that dies shows up as
  `PeerClosed` on the output and the session ends with exit status 139
  (scenario `ssh.shell_fault`); the shell, holding only the receive side of
  its input, sees `PeerClosed` when sshd goes.
- Latent: fsd waits with `ipc_receive_any` on its main endpoint and the
  granted namespace endpoints, and exits on `Revoked`, which the call
  reports when any one of them is revoked. Nothing revokes a namespace
  endpoint today; if something starts to, fsd must drop that endpoint
  instead of exiting.
- Kernel threads: a capability a kernel thread drops outside a syscall and
  outside `reap()` (self-tests) is settled at the next syscall or reap on
  any CPU.
- `endpoint_create_pair` packs two 32-bit handles into the 64-bit result
  (`receive | send << 32`). A handle is `generation << 16 | (index + 1)`,
  so the value is negative as an `i64` whenever the send handle's
  generation is >= 0x8000; that is harmless, since only -4095..=-1 reads as
  an error, which needs the send handle to be 0xFFFF_FFFF and the receive
  handle >= 0xFFFF_F001: an index of at least 0xF000 in both, far above
  `HANDLE_LIMIT` (256). Asserted in debug builds.
- Releasing a reaped process's capabilities runs in `reap()` with
  interrupts disabled, like the address space release it follows. It is
  bounded (at most 256 capabilities; a closing endpoint discards at most 16
  messages of 4 capabilities each, iteratively) and measured: above 1 ms
  it is logged and recorded as a `slow capability release` fault event.
  Capabilities are released and their endpoints settled before the process
  counts as reaped, so `process_wait` returns after they are gone.
- Not covered: a server alive but stuck (it holds the request and never
  answers) still costs the full wait; the client's deadline is not carried
  in the protocols (incident 0001, open item). Reference cycles through
  queues leak: an endpoint whose own `READ` capability sits in its queue
  (directly or through other endpoints' queues) never loses its receive
  side, so neither closes nor is freed. This was already an `Arc` cycle
  before; counting does not make it worse, and breaking it needs a
  collector or a rule against queuing an endpoint's receive side on
  itself.

## Resolution of audit 1 (`docs/audits/robustness-audit-1.md`)

- B1: services stop using an endpoint that is gone (consequence above;
  refined by the re-audit, N1: only the send side is permanent).
- B2: history rebuilt so every commit builds; this ADR renumbered to 0030.
- B3: the QEMU suite is run centrally.
- M1, L4, L5: sshd (ADR 0025 note).
- M2, M3, L1, L3: kernel (consequences above; `settle_closed` loads the
  flag before swapping it).
- M4: pre-existing, incident 0004, not fixed here.
- L2, L7: documented above. L6: init's probes use reply pairs. L8: the
  SMP self-test warns when migration takes over 1 s (incident 0002).
- Reviewer 1: init does not close the endpoints of a service it could not
  kill (it may still be serving).

## Rejected alternatives

- **Counting `Arc` references** instead of capabilities: the kernel holds
  references that are not capabilities (self-tests, IRQ tables), and one
  reference does not say whether it may receive.
- **A unique receive right that returns to a keeper when its holder dies**
  (Mach port-destroyed notifications): exact for supervisors, but a new
  object model; init keeping a `READ` capability gives the same behaviour
  during restarts, and `endpoint_close` covers giving up.
- **The kernel closing the endpoint when "the serving process" dies**: the
  kernel cannot tell init's capability from the server's, and closing an
  endpoint while a restart is pending would break ADR 0011.
