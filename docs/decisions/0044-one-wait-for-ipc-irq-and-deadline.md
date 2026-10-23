# ADR 0044: One wait for IPC, IRQ and deadline

- **Date:** 2026-10-23
- **Status:** Accepted (performance audit of 731c398, finding B and
  factorization order 1; extends ADR 0020; amends ADR 0024 and ADR 0035's
  round, which keeps its budgets)

## Context

A service that drives a device and serves clients (netd, the input
drivers) had one thread and two things to wait for: its endpoints and its
device's interrupt line. `ipc_receive_any` (ADR 0020) waited on endpoints
only and `irq_wait` on one line only, so the loops polled their endpoints
with timeout 0 and then slept in `irq_wait(IRQ, 10)`. A request that
arrived after the poll did not end that wait: it waited for the next
interrupt or for the 10 ms. netd's metadata calls, which send no frame and
so cause no interrupt, paid the whole timeout: the performance audit of
731c398 measured `NetClient::info` at p50 10,002 µs on one CPU and 10,001
µs on four, against 58 µs for fsd's `list`, which already waited on all its
endpoints at once. Requests left queued past the round's budget of eight
also slept until then. And an idle netd, virtio-input or usb-input woke
100 times a second to find nothing.

Shortening the timeout or spinning trades CPU for latency without fixing
the wakeup: the audit asked for a wait that ends on whichever comes first
(a message on any endpoint, the device's interrupt, the service's next
deadline) and is not entered while a round left work, shared by the
service loops instead of repeated in each.

## Decision

1. **`ipc_receive_any` also waits on interrupt lines** (ABI 6). Its set may
   hold, besides up to `IPC_MAX_ANY = 16` endpoints (`READ`), up to
   `IPC_MAX_ANY_IRQS = 4` interrupt lines (`WAIT`), every member checked
   before waiting; only lines the caller holds with `WAIT` can be listed,
   as for `irq_wait`. It returns the first ready member in list order:
   an endpoint's oldest message, as before, or a line's interrupts
   accumulated since they were last taken, which the call takes (and
   unmasks a level-triggered line) exactly as `irq_wait` does. The result
   for a line is `count | RECEIVE_ANY_IRQ (bit 47) | index << 48`; a
   message keeps the ABI 5 layout (`aricode_abi::receive_any` encodes and
   decodes both). Extending the existing call was preferred to a new wait
   object: a service names what it waits on with the capabilities it
   already holds, nothing is created or torn down per wait, and an
   endpoint-only set behaves, and fails, exactly as before (anything that
   is not a line is checked as in ABI 5, rights before kind: without
   `READ` it is `Denied`, with `READ` but not an endpoint `Invalid`).
   The version goes to 6 because a program that lists a line needs a kernel
   that accepts it; an older kernel answers `Invalid`.
2. **Wakeup semantics.** The waiter registers on the receive queue of
   every endpoint in the set and on each line's own queue before it checks
   the set, with interrupts disabled from the registration to the block
   (`task::wait::wait_any_until_deadline`). A send wakes the oldest waiter
   of its own endpoint (`wake_one`, whether that waiter waits on the
   endpoint alone or on a set holding it) and an interrupt the waiters of
   its own line: nothing else in the system. ADR 0020's global
   `ANY_RECEIVERS` queue, which every send woke, is gone (see "Measured
   on the way" below for why it could not stay). No wakeup is lost: a
   waker changes the state and then takes the queue's lock to wake it; if
   it takes the lock after the registration it finds the waiter there, and
   if before, its change is visible to the check (the lock orders both). A
   send's `wake_one` can pick a set's waiter that is already awake for
   another member; the waiter then takes that other member and the wakeup
   would be lost for the endpoint's other receivers, so once the waiter has
   left every queue and taken its member it passes a wakeup on
   (`wake_one`) for every other endpoint of its set that still holds a
   message. A line's count is a latch (`IrqLine::pending`): an interrupt
   that fired while nobody waited, during the service's round, leaves the
   line ready and the next wait returns at once. A wake between the check
   and the block turns the block into a yield, as for one queue. The
   waiter leaves every queue when the call returns, so no stale entry
   wakes it from a later wait on another object. FIFO order among the
   waiters of an endpoint and the list-order rule between members are
   kept; the cost is bounded by the set (at most 16 + 4 queues to register
   on and leave, and as many hand-on checks). A poll (timeout 0) with
   nothing ready returns without registering.
3. **`PeerClosed` keeps its meaning.** It is reported when no member is
   ready and every endpoint in the set is empty and has lost its send side
   (ADR 0030). A line never closes, but a service whose endpoints are all
   gone has nobody left to serve, so a pending interrupt is returned first
   and then `PeerClosed`; a set of lines alone never reports it.
4. **The SDK's `wait` module is the loop's end** (`sdk/rust/src/wait.rs`):
   `Deadline` keeps the earliest of a service's deadlines, `timeout` turns
   it into the wait's timeout (`None`: do not wait, because the round's
   budget left work or the deadline is due; no deadline: no timeout), and
   `end_of_round` waits on a `Members` set (endpoints first, lines after
   them) and reports a message, which the caller serves, an interrupt or
   `Round`. The decision is host-tested; `wait_any` is the raw call.
5. **netd** ends every round (ADR 0035's: up to `FRAMES_PER_ROUND = 32`
   frames, up to `IPC_REQUESTS_PER_ROUND = 8` requests from the main
   endpoint and every session, the timers) with one wait on all its
   endpoints and the NIC's line, skipped when frames were left in the ring,
   when the request budget was used up or when a sweep took requests that
   are still to be served. Its timeout comes from `Stack::next_deadline`:
   the periodic sweep (1 s, so an idle netd wakes once a second instead of
   a hundred times), DHCP (retry or the ARP probe of an acknowledged
   address), pings, DNS, UDP and TCP receives, accepts and connects, each
   TCP connection's retransmission or TIME-WAIT end
   (`tcp::Connection::next_timer`), frames parked for ARP and faults held
   back by their one-second window (`aricode_faults::Meter::next_due`). A
   request that ends the wait is served at once, with the clock read again
   (the wait may have slept until a deadline: a deadline the request sets
   counts from its arrival). A wait that fails for any other reason than the endpoints being gone
   exits with code 10 instead of looping on the error.
6. **virtio-input and usb-input** end their rounds the same way: their
   endpoints (usb-input's block service too) and their line, until the
   waiting read's deadline. usb-input adds its recovery steps
   (`Controller::next_maintenance`: a refused Disable Slot or a due class
   reset at once, the next GET_STATUS poll after a reset, a
   re-enumeration's `not_before`, a faulted HID endpoint's `not_before`).
   A due step can be unable to progress until the controller completes a
   command (a re-enumeration postponed behind an outstanding Disable Slot),
   so maintenance deadlines are waited for at least `MAINTENANCE_PACE_MS =
   10` ms, the pace the loop had before; without recovery in progress the
   driver sleeps until a request or an event. virtio-input no longer stops
   draining its event queue at a slot that held no valid event, which
   would now leave the events behind it until the next interrupt.

## Measured on the way

The first version kept `ANY_RECEIVERS` for the endpoints of a set and
added only the lines' queues. With netd, virtio-input and usb-input now
waiting on sets, every message in the system woke all of them (and fsd)
to re-check queues that had not changed: finding E of the audit, which
before cost little because only fsd waited there. The area run caught it
in `usb.hid_fault`, which timed out: its `diag` took 9.7 s instead of 4.0 s
on one CPU (the shell's dozens of requests to fsd, init and netd each woke
the other services), typing took 1.4 times as long, and in the same
configuration fsd's `list` cost 74 µs instead of 56. Registering on each
endpoint's own queue (point 2) removed the cost instead of the test's
margin.

## Rejected alternatives

- **A shorter timeout, or spinning.** Latency for CPU; the request that
  arrives after the poll still waits, only less, and an idle service wakes
  more often.
- **A new wait-set object** (create, add, wait, destroy). More ABI and
  kernel state per service for nothing the existing call cannot express;
  the capabilities a service holds already name its members.
- **Keeping ADR 0020's `ANY_RECEIVERS` for the endpoints** (and having
  interrupts wake it too, simpler still). Every send in the system would
  wake every service waiting on a set, see below.
- **Waking every waiter of the endpoint on a send** instead of passing the
  wakeup on. Also correct, but with several receivers on one endpoint
  the oldest would no longer be the one that gets the message.
- **A second thread per service for the interrupt.** There are no user
  threads, and the stack would need locking.

## Consequences

- ABI version 6. Every endpoint-only caller (fsd, the EHCI driver, the
  test programs) is unchanged; `ipc_receive_any` in the SDK stays the
  endpoint-only form and `wait_any` is the general one.
- `test=wait_set` (`ipc.wait_set` on one CPU, `smp.wait_set` on four):
  interrupts latched before the wait return at once with their count; list
  order decides between a message and an interrupt, both ways; a 50 ms
  deadline holds; another thread's interrupt or message ends a blocked
  wait; 200 interrupts fired by a helper (pinned to another CPU on SMP)
  around the moment the waiter blocks are all reported, none lost; a
  send's wakeup taken by the set's waiter for another member is passed on
  to the endpoint's single receiver (without the hand-on that receiver got
  its message only at its 5 s deadline: checked); `PeerClosed` with a line
  in the set; no registration is left behind.
  `pit_driver` (`drivers.user_irq_mmio_dma`) checks the syscall path with
  the real PIT: the line ends a wait on an empty endpoint, a queued
  message listed first wins, a line without `WAIT` is `Denied`, a process
  handle `Invalid` and five lines `Invalid`. Host tests: the result
  encoding (`aricode-abi`), the wait decision and `Members` (`aricode-sys`),
  `Connection::next_timer` driving retransmission, TIME-WAIT and give-up
  with time jumping from timer to timer (`aricode-net`) and `next_due`
  (`aricode-faults`).
- `bench` measures the network-info call and the idle time of every CPU
  over a quiet 2 s interval. Network info p50: 10,001 µs before, 71 µs
  after on one CPU; 9,999 and 74 µs on four. Quiet idle: 986 to 989
  permille on one CPU, 992 to 997 on four. fsd's `list` costs about 4 µs
  more on one CPU (a wait registers on each of fsd's endpoint queues); the
  details are in `docs/performance.md`.
- Deadlines still have the kernel's tick granularity (10 ms): a timeout
  ends at the first tick at or after it. Messages and interrupts end a wait
  at once.
- Not changed: the compositor reads its input services one after another
  with 1 ms reads (finding B's last paragraph); waiting on them at once
  needs reads kept outstanding on reply endpoints in one set with its
  service endpoint, a change of its own. The EHCI driver polls its
  controller (`POLL_MS`) and keeps its loop. Device interrupts arrive on
  the boot CPU, as before.
- Found by this change's suites and fixed in the same branch: usb-input
  read an xHCI event whole before its cycle bit and could take a stale
  event for a new one (incident 0024; exposed with 4 vCPUs once block
  requests were served at once), and `test=drivers` checked a pixel the
  kernel console repainted on its next scroll (one more boot line put
  the 4-CPU run there; the self-test now lends the screen to the driver).
