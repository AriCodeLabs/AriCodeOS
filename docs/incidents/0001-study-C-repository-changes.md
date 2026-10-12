# Incident 0001, study C: what changed between fc846ae and HEAD

Read-only review of `git diff fc846ae..11ccbdc` over the kernel, the USB
driver, the xHCI and storage libraries, the block client, init, the
runtime and the new clients (sshd, updater, shell).

## Perimeter (facts)

- The kernel diff is 7 files / 165 lines: `random::stir()` from the
  device, serial and keyboard interrupt handlers; `rtc.rs` read once at
  boot; idempotent `map_foreign`. Nothing in the scheduler, timers, IPC,
  frame allocation, APIC/MSI, IOMMU or process exit changed.
- The driver transfer path is byte-identical between fc846ae and the first
  failing image 653e12f: only `ClientBuffers` became `map_client_buffer`
  (9 lines). `bulk()`, `scsi()`, `bulk_trbs`, the rings, the endpoint
  contexts, `recover_bulk` and `poll()` are unchanged. Every later driver
  change is a reaction to the incident, not a candidate cause.
- What is new is the workload: new processes write to AriFS at boot and
  issue further commands at machine speed (sshd, `update URL`, the
  updater). The fc846ae day was human-paced.

## R1 (most likely): the workload exposes a stick that fails handshakes for a while after a WRITE

Every transcribed failure follows a write by milliseconds: sshd writes
`random_seed` (and possibly the host key) then reads `authorized_keys`;
`update URL` writes the channel file and launches the updater, whose first
command reads LBA 1; the updater creates `last.log` then writes its
content. Code 4 with CErr = 3 means three consecutive bus errors: a
device that does not handshake for even 100 us after returning the WRITE
CSW produces code 4 on the first packet of the next command (the CBW
OUT); a NAKing device would not. Skepticism: `diag NAME` on fc846ae also
did metadata-then-data writes at machine speed without failing, so the
window may be probabilistic, size-dependent, triggered by read-after-write
specifically, or hardware may be in play.

Decisive experiment (do first): re-flash the fc846ae image on the same
stick and port today and drive a scripted write immediately followed by a
read; then HEAD with the identical sequence. Without a same-day A/B the
correlation with the commit date is confounded by the stick's state.

## R2 (certain bug, pre-existing, now load-bearing): the endpoint is not un-halted after a USB Transaction Error

Same finding as study B H2, with the same lines; the Bulk-Only reset is
the one recovery this stick cannot survive, and the working tree postpones
the only resynchronising step to the sixth failure. Fix: treat every
non-success completion on a bulk endpoint as Halted (Reset Endpoint + Set
TR Dequeue + CLEAR_FEATURE(HALT) on the failed endpoint) before any retry;
no Bulk-Only reset for code 4; escalate to a port reset with
re-enumeration (the driver has no re-enumeration path today). Host test:
inject completion codes into `scsi()`'s result path and check the
commands issued.

## R3 (certain design flaw from the retry commits): the server's retry budget exceeds the client's 5 s reply timeout

`BlockClient` waits `rpc::GRACE_MS` = 5000 ms; `block_io` can spend
5 s + 1 s + 2 s per attempt, up to 7 attempts, plus pauses. The client
gives up at 5 s and closes its reply endpoint; the server's late reply
fails silently. Consequences: fsd returns `Io` while the write may later
succeed (`last.log` created with 0 bytes; `update check` "no UPDATE
partition" while `ls` works seconds later); the SSH banner delay is this
budget; and the client reuses the same DMA buffer for its next request
while the server's late retry still DMAs the old one. Buffer frames
survive while the server holds its handle, except in the timeout path:
`bulk()` returns with the TD still on the ring, the handle is closed, and
if the client is gone the frames are freed while the controller can still
complete into them.

Fix: bound the server's total time per request below the client's wait
(or pass the server's worst case as the client's wait); Stop Endpoint +
Set TR Dequeue before releasing a timed-out buffer; record a fault event
when the reply cannot be delivered.

## R4 (pre-existing, low probability): a 2-TRB TD straddling the Link TRB has no chain bit on the link

`Trb::link` never sets CH and `ProducerRing::push` does not carry the
previous TRB's chain bit into the link (xHCI 4.11.5.1 requires it). Only
16-sector clients (init, updater) with buffers straddling a 64 KiB
boundary (1 in 16 allocations) can hit it; it would give code 5, not 4.
Fix regardless.

## R5 (analysed, cannot cause code 4): per-request `map_client_buffer` + idempotent `map_foreign`

The server never touches the client buffer through its virtual mapping;
only the physical address reaches the TRBs. `map_foreign` matches on
page-aligned start and span and mappings are never removed before exit,
so a stale address is impossible; cacheability cannot mismatch. The old
cache had a latent bug the new code fixes (stale length for a reused
physical address). Residual issue: foreign mappings of freed DMA buffers
stay mapped in the servers.

## R6 (analysed, dismissed): the other changes

`random::stir()` is one `rdtsc` and two relaxed atomics before the vector
lock; `rtc::init()` is a few CMOS reads at boot; init waiting for sshd,
the DHCP probe and TLS load only change user-process timing; the
`sleep_ms` in `block_io` only delays other clients after a failure. None
reaches the USB bus.

## What to do, in order

1. Same-day A/B on the stick with fc846ae and HEAD under a scripted
   write-then-read.
2. Land the strict-dump instrumentation (phase, direction, previous
   command and ms since its CSW, event parameter vs expected TRB, TRB count
   and ring index, PORTSC, Port Status Change events, fault event when the
   client is already gone).
3. Fix R2 and R3 with host tests of the state machine; keep any post-write
   quiet period only as a documented, counted workaround if the A/B
   confirms R1.
4. Fix R4 as a compliance correction.
