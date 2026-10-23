# Incident 0024: an xHCI event read whole before its cycle bit

- Opened: 2026-10-23. Status: fixed on `feature/service-wait-set`.
- Found by: `cargo xtask test --all --cpus 4` of `feature/service-wait-set`
  (ADR 0044), scenario `usb.storage`, boot 1; the previous `--cpus 4` run
  of the same driver code and every 1-CPU run passed.
- Environment: QEMU TCG with 4 vCPUs, qemu-xhci with USB storage; real
  controllers write the TRB from the device, with the same exposure in
  principle.

## Evidence

```text
fs_client: hola.txt says: Hola desde AriFS
usb-input: stray transfer event: slot 1 dci 3 completion code 1 (Success) trb 0x5c7100 residual 0
usb-input: DATA IN dci 3 failed: completion code 0 (no event) after 30008 ms, no event, td 0x5c7140..0x5c7140
  ep 1 Running dequeue 0x5c6c60; ep 3 Running dequeue 0x5c7150; ep 4 Running dequeue 0x5c7250;
usb-input: CSW received after the failure: status 0 residue 0; transport ready
usb-input: READ(10) lba 360 transport failure
```

The controller had completed the TD at 0x5c7140 (endpoint 3's dequeue
pointer is already past it, at 0x5c7150) and the event ring went on
working (the CSW arrived), but the driver never saw that TD's event: in
its place it read a Success event for 0x5c7100, an earlier TD of the same
endpoint, which it reported as stray, and then waited the full 30 s.

## Mechanism

`Controller::next_event` read the 16-byte event TRB at the dequeue slot in
one volatile load and then checked its cycle bit (`EventRing::accept`).
QEMU's xHCI writes the event from its device thread while a vCPU may be
reading the slot, and TCG performs a guest 16-byte load as two 8-byte
host loads. A read that straddles the write returns the new last dword
(cycle bit of this lap, completion code) with the first 8 bytes the slot
held one lap earlier (that older event's TRB pointer). The cycle bit says
"new", so the driver accepts it and advances: the real event is consumed
as a stale one. The window is a few instructions wide, which is why it
needs several vCPUs and a driver that reads the ring while the controller
is writing it. ADR 0044 made that more likely: usb-input now serves the
next block request as soon as it arrives instead of up to 10 ms later.

## Fix

`EventRing::take` (`libs/xhci`) reads the slot's last dword (the one with
the cycle bit) first and the whole TRB only once that dword says the
controller wrote it for this lap; `next_event` passes it two volatile
loads, which x86 keeps in order (and TCG keeps in order for an x86 guest
on an x86 host). Once the cycle dword is new, the write of the TRB has
reached it, and the rest read after it is the new event's.

## Tests

- Host (`libs/xhci`): `event_ring_reads_the_cycle_dword_first`: a slot
  whose cycle dword is stale is not read at all, a fresh one is taken
  once.
- QEMU: `usb.storage` and the `usb` area with 1 and 4 CPUs, and the full
  `--cpus 4` suite of the branch.
- The race itself cannot be forced from a scenario: it depends on the
  host scheduling QEMU's device thread against a vCPU.
