# Incident 0017: a buffer the device may still own is released after a failed request

- Opened: 2026-10-21. Status: reproduced (commit `6b7fbc9`) and fixed on
  branch `fix/device-owned-buffers` (commit `09609a4`); audited
  (2026-10-21, nothing serious; follow-ups in "Audit"); pending merge. Not
  yet exercised on the reference laptop.
- Found by: the second independent audit of `fix/dma-mapping-lifetime`
  (incident 0004), finding M1. Predates that branch; the branch delays it
  (bounded client-buffer cache) but does not fix it.
- Environment: any; harmful without an IOMMU (the reference laptop has
  none). With an IOMMU the late write becomes an IOMMU fault.

## What the code did

- virtio-blk: `request()` returned an error on timeout and left the
  descriptor chain in the ring, and the server closed the client's buffer
  handle anyway (`services/virtio-blk/src/main.rs`, after `dev.request`).
  If the client then closed its own handle and eight later requests
  evicted the cached mapping, the buffer's frames returned to the PMM
  while the device could still complete the old request and write into
  them. The stale chain also stayed in the ring for good: the next
  completion the driver saw was the old request's, so the request after a
  timeout failed too (`done != head`), and so on.
- usb-input (xHCI): `slot_disabled` closed `held_buffer` on any Disable
  Slot completion, even when the slot was not released and the controller
  could still use the buffer (`services/usb-input/src/main.rs`, around
  line 1164). Worse, when the Disable Slot completed at once (the usual
  case) nothing was held yet: `retire_failed` stayed false and
  `serve_block` closed the buffer whatever the completion code.
- usb-ehci: a client buffer whose queue head the controller never
  released (`Fail::Lost`) was already kept for good (`buffer_unsafe`), but
  only printed, not recorded.
- The network drivers copy into their own buffers; only the block servers
  hand a client's buffer to a device.

## Reproduction (2026-10-21, commit `6b7fbc9`)

Two test-only injections (`aricode_cmdline::keys::TEST_ONLY`, refused by
`cargo xtask usb-image`) and two scenarios:

- `blk_fault=LBA`: virtio-blk does not notify the device of the first
  client request that covers `LBA` (a lost doorbell). The request then
  times out for real after 2 s with its chain in the ring, and the device
  processes it at the next notification. With it on the command line (and
  `selftest`), init runs the new test client `blk_late` on that LBA:
  1. read the LBA into buffer A (4 KiB): `Io` after the timeout;
  2. close A, then send eight requests with eight other 4 KiB buffers and
     16 sectors each: the server maps each one (its `ClientBuffers<8>`
     evicts A) and refuses it as too small without touching the device;
  3. allocate eight fresh buffers (the PMM gives the lowest free frame
     first) and fill them with 0x55;
  4. send a valid read: the device is notified and processes the ring;
  5. fail with exit 20 if any fresh buffer changed, 21 if the valid read
     failed.
  virtio-blk got `SYSTEM` (init's `with_system`) to read its command line
  (since the audit only when `blk_fault` or `strict` is on it).
- `usb_fault=...:unreleased`: like `reenum` (the storage endpoint is given
  up, Disable Slot, re-enumeration), with the Disable Slot completion read
  as Context State Error, a code after which the controller may still own
  the slot's rings (xHCI 4.6.4).

Debug checks, in the same commit: virtio-blk printed and recorded when it
closed a client buffer while descriptors were in flight
(`Virtqueue::in_flight`, new, host-tested); usb-input when it closed one
after a Disable Slot that did not release its slot.

`cargo xtask test --level full` on the two scenarios, 1 CPU, before the
fix. `storage.late_completion` with `iommu=off` (untranslated DMA, as on
the laptop), `build/scenarios/storage.late_completion/serial.log`:

```text
blk_late: reading LBA 4300 into buffer A at 0x487000 (the device is not notified)
virtio-blk: injected fault: the device is not notified of this request (blk_fault)
virtio-blk: client buffer released while the device still owns 3 descriptor(s) (incident 0017)
blk_late: the request timed out (Io), as injected
...
[INFO ] user: process 6 dropped the last mapping of a DMA buffer it had no handle to (4 KiB at 0x487000): its frames return to the PMM now
blk_late: buffer A closed and pushed out of the server's mapping cache
blk_late: a fresh buffer got the frame of buffer A
blk_late: a late device write landed in a fresh buffer at 0x487000: disk data (incident 0017)
[INFO ] user: process 10 'blk_late' exited with code 20
```

The frame of the timed-out buffer went back to the PMM, was handed to
another buffer, and the device's late completion wrote sector 4300
(`ARIDISK\0`...) into it: memory the device was never given. Any owner
could have received that frame (a page table, the kernel heap). With the
emulated IOMMU on, the same run ends in
`iommu: DMA fault from 00:02.0 (write at 0x49e000): write to page without
write permission`: the IOMMU entry went with the buffer. The debug check
fired on the timed-out request and on every later one (the stale chain
never left the ring).

`usb.unreleased_slot`, `build/scenarios/usb.unreleased_slot/serial.log`:

```text
usb-input: injected fault: the Disable Slot completion is read as Context State Error
usb-input: Disable Slot slot 1: completion code 19 (Context State Error)
usb-input: slot memory abandoned: Disable Slot did not release the slot (completion code 19 (Context State Error)); 2336 bytes of the DMA arena stay allocated
usb-input: client buffer released although its slot was not released (incident 0017)
```

The slot's own memory was abandoned (correctly) while the client's buffer,
which the same TDs pointed at, was released. QEMU's xHCI does not keep
writing after a Disable Slot, so this one shows the decision, not a
corrupted frame.

## Mechanism

Without an IOMMU the only thing that keeps a device from writing into a
frame is that the frame stays allocated to the buffer it was given. A
driver's hold on a client buffer is its handle (and, since ADR 0038, its
mapping, which the bounded cache drops on its own schedule). Both drivers
gave the handle up on paths where the device had not given the buffer
back: virtio-blk on every failed request, usb-input on every Disable Slot
completion. After that, only the cache's mapping kept the frames, until
eight other buffers came along.

## Study

Done by one agent reading the drivers, the SDK transport and cache, the
kernel's `mmio_map`/`mmio_unmap` and `DmaBuffer::drop`, and the virtio
and xHCI specifications; not the three independent reviews
`docs/FAILURE-PROTOCOL.md` asks for. The reproduction confirms the
mechanism; the audit step remains. Points settled:

- **A handle is enough to pin a buffer.** A handle holds the `DmaBuffer`
  object; its frames and its IOMMU entries go only when the last
  reference goes (`kernel/src/hw.rs`, `Drop for DmaBuffer`), and
  `mmio_unmap` leaves the IOMMU entry (ADR 0038, "IOMMU"). So a driver
  keeps the handle and lets the mapping cache evict freely; pinning the
  mapping as well would add nothing.
- **When a virtio device stops owning buffers.** Only when it returns
  them through the used ring or after a reset: the driver writes 0 to
  `device_status` and the device presents 0 once the reset is done
  (virtio 1.2, 4.1.4.3.1), back in its initial state with its queues
  disabled and sending nothing until `DRIVER_OK` (2.4.1). Linux frees
  virtqueue memory on the same basis (`virtio_reset_device` before the
  queues are deleted). A per-queue reset (`VIRTIO_F_RING_RESET`, 2.6.1)
  would do with less, but QEMU's and the drivers' negotiation do not use
  it.
- **When an xHCI stops owning a slot's TDs.** After a Disable Slot that
  completes with Success (or Slot Not Enabled: there was nothing),
  `completion::disable_slot_released` (xHCI 4.6.4). After any other code
  nothing says it ever will: usb-input has no controller reset after
  boot, so the buffer is kept for good, as the slot's own memory already
  was.

## Fix (commit `09609a4`)

- `sdk/rust/src/client_buffer.rs`: `DeviceOwned<N>` keeps the handles of
  client buffers a device may still own; `release` closes them once the
  device cannot reach them, `abandon` and `keep_for_good` keep them for
  good (never closed, counted). Host tests.
- `sdk/rust/src/virtio_pci.rs`: the reset is a bounded `Transport::reset`
  (`SetupError::ResetTimeout`; it was an unbounded spin; 100 polls 10 ms
  apart since the audit, 1 to about 2 s with the 100 Hz tick), the
  negotiation is `Transport::negotiate`, and `Transport::restart_queue`
  creates a queue again, empty, on its own memory after a reset.
- virtio-blk: after each request the driver asks the ring whether the
  device owns anything (`in_flight() > 0`, or `DEVICE_NEEDS_RESET`). If
  it does, the client's buffer handle is kept (`blk buf kept`, with the
  LBA), the client gets `Io`, and the device is reset, renegotiated (the
  features must not change), its queue created again and `DRIVER_OK` set
  (`blk reset`); only then is the buffer released (`blk buf released`).
  The stale chain goes with the reset, so the next request works again.
  Bounded: three resets with no request completed in between, a reset
  that does not finish, or a feature change make the driver give the
  device up and exit (`blk given up`); the kernel then clears bus
  mastering before it frees what the driver held, the kept buffer
  included (ADR 0038, `stop_claims_of`), and the supervisor restarts the
  driver, which starts with its own reset. The GPT reads at start-up go
  through the same check.
- usb-input: the buffer of the request being served has a fate
  (`BufferFate`): `Release`, `Hold` (set by `give_up_endpoint` before its
  Disable Slot) or `ForGood`. A Disable Slot completion that released the
  slot releases what was held for it (`usb buf released`) and turns `Hold`
  into `Release`; any other completion keeps them for good (`usb buf for
  good`) and turns `Hold` into `ForGood`. A Disable Slot superseded by
  another (the older one never completed) keeps its buffers for good. The
  reply path settles the buffer (`usb buf kept` when held). `held_buffer`
  and `retire_failed` are gone.
- usb-ehci: keeping a buffer under a lost queue head for good now records
  `ehci buf for good`.

Why no `aricode_faults` meter: the events cannot come faster than the
requests that cause them, and each one waits out a timeout first (2 s in
virtio-blk, the transfer windows and a failed Disable Slot in usb-input);
the meter's window is 1 s, so it would never hold one back. What bounds
them is the recovery: virtio-blk gives up after three resets in a row;
usb-input gives a storage device up after `REENUMERATE_ATTEMPTS`; usb-ehci
marks the storage unusable on the first lost queue head.

## Tests

- `storage.late_completion` (new, two boots): with `iommu=off` the
  scenario fails on `6b7fbc9` (log above, exit 20) and passes on `09609a4`:

  ```text
  virtio-blk: injected fault: the device is not notified of this request (blk_fault)
  virtio-blk: client buffer kept until the device is reset: its request did not complete
  virtio-blk: resetting the device: a request did not complete
  virtio-blk: client buffer released: the device was reset
  blk_late: the request timed out (Io), as injected
  [INFO ] user: process 6 dropped the last mapping of a DMA buffer it had no handle to (4 KiB at 0x487000): its frames return to the PMM now
  blk_late: buffer A closed and pushed out of the server's mapping cache
  blk_late: a fresh buffer got the frame of buffer A
  blk_late: no late write reached reused memory; the next request succeeded
  ```

  The frame is reused here too, but after the reset, when the device no
  longer knows the chain. With the IOMMU on, the second boot shows no DMA
  fault.
- `usb.unreleased_slot` (new): fails on `6b7fbc9` (`client buffer released
  although its slot was not released`, a message of the debug check that
  the fix removed), passes on `09609a4` with `client buffer kept for good:
  its slot was not released` and `usb buf for good` in `diag`; the stick
  is re-enumerated and the next write succeeds.
- Host tests: `aricode-sys` (`DeviceOwned`: held until released,
  abandoned handles never closed, overflow kept for good), `aricode-virtio`
  (`in_flight` over a chain's life), `aricode-cmdline` (`blk_fault` is
  test-only).
- `cargo xtask check` clean; `cargo xtask test --all`: 162 QEMU scenarios, 0 failed;
  `cargo xtask test --all --cpus 4`: 162 QEMU scenarios, 0 failed (both on `09609a4`).

## Open

- The give-up path of virtio-blk (three resets in a row, a reset that
  does not finish) is not exercised by a scenario; QEMU's virtio-blk
  always finishes its reset at once.
- usb-input's `Hold` path (a Disable Slot that is still pending when the
  request is answered, released by its late completion in `dispatch`) is
  not exercised either: QEMU completes Disable Slot at once.
- Buffers kept for good by the USB drivers stay pinned until the driver
  exits: one client buffer per failed Disable Slot or lost queue head,
  the client's whole buffer, still charged to the client that allocated
  it (ADR 0038, "Who pays"). A controller reset (HCRST, HCRESET) after
  boot would let them go; neither driver has one.
- Long run: each buffer kept for good is one handle in the driver's
  table (`HANDLE_LIMIT`, 256) as well as pinned memory. In usb-input
  that is at most one per failed Disable Slot, so about plug cycles x
  `REENUMERATE_ATTEMPTS`; in usb-ehci one per lost queue head, so one
  per plug cycle.
- `strict` in virtio-blk (below) has no QEMU scenario yet.
- Not yet run on the reference laptop.
- The study was done by one agent; the audit below was independent.

## Audit (2026-10-21, commit `24d4a77`)

An independent audit verified that the handle pins the frames and the
IOMMU entry, the reset-then-release order, stop-before-free on the
give-up path, the gating of `blk_fault` and that `BufferFate` covers every
path. Nothing serious; the follow-ups, in the commit after `24d4a77`:

- **Tests did not tell "keep until reset" from "release, then reset".**
  Moving the release before the reset, or closing the handle in
  `settle_client_buffer`, still passed (`serial_contains` is unordered).
  `storage.late_completion` now has a `serial_order`: kept, resetting,
  `device reset done` (new, printed once the device is back), released,
  then the frame's return to the PMM (blk_late's own lines are left out:
  with several CPUs it prints its reply while the driver resets).
  `DeviceOwned` takes its close function at construction, so the host
  tests check that `release` closes every held handle and that the
  overflow, `abandon` and `keep_for_good` never close one.
  `usb.unreleased_slot` asserted the absence of a message no code prints
  any more; it now checks the order (the Disable Slot's verdict, then the
  buffer kept for good, then the write error).
- **The reset bound was 10 to 20 s, not 1 s**: `thread_sleep(1)` sleeps at
  least one tick plus one. Now 100 polls of `thread_sleep(10)` (1 to
  about 2 s); a device that reads 0 at once (QEMU) costs no sleep, so the
  virtio-net and virtio-input boots are unchanged.
- **Least privilege**: virtio-blk got `SYSTEM` on every boot for a
  test-only key. init now passes it only when the command line has
  `blk_fault` or `strict`; without it `sys::cmdline` returns `None`.
- **`strict` was ignored by virtio-blk** (FAILURE-PROTOCOL rule 3). With
  `strict` the first failed request halts the driver (`strict: halting on
  a failed request`, with the error code, the descriptors in flight and
  the device status; `blk strict halt`; exit 52), before any reset and
  with the client's handle still open: the reap clears bus mastering
  before anything is freed and init restarts nothing.
- **usb-input released `owned` on any slot's completion.** `Hold` now
  carries the slot, and the kept buffers remember theirs (`owned_slot`):
  only that slot's Disable Slot releases them or keeps them for good; a
  request whose slot differs from the one already held is kept for good.
  Unreachable today (one Disable Slot at a time), cheap hardening.
- **A request without data** (Flush, or one refused before the device)
  that carried a handle had it kept needlessly when the device owned
  something; such a handle is always closed now.

After the follow-ups: host tests green and `cargo xtask check` clean; the
QEMU suite is run by the coordinator on that commit.
