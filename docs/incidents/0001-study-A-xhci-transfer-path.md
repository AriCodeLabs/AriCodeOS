# Incident 0001, study A: xHCI transfer path

Read-only review of `libs/xhci/src/lib.rs` and
`services/usb-input/src/main.rs` against xHCI 1.1.

## Summary

1. TD/TRB construction, cycle bit and link handling and the event ring
   consumer are correct; they are not a plausible source of completion
   code 4. The only deviation (Link TRB without the Chain bit when a TD
   spans the wrap) is benign for these transfers.
2. What is wrong is the endpoint state machine after an error: a halted
   endpoint is recovered only after STALL and only on some stages; Reset
   Endpoint and Set TR Dequeue are issued to endpoints that are not
   halted; the completion codes of those commands are ignored; EP0 is
   never recovered; timed-out TDs are never retired (no Stop Endpoint);
   error events are matched without checking the TRB pointer; Port Status
   Change bits are cleared without being read. Each converts one transient
   transaction error into multi-second timeouts, toggle desynchronisation
   or a permanently dead slot. That is the amplifier; the trigger is
   almost certainly the device or the physical layer, which only correct
   handling can address.
3. The only driver change between fc846ae and 653e12f in this path
   (`ClientBuffers` to `map_client_buffer`) alters no DMA address or TRB.

## H-A (high confidence, the amplifier): a code 4 on the IN endpoint leaves it Halted and nothing recovers it

`scsi()`: a CBW failure recovers for any code; the data stage recovers only
on STALL; the CSW stage recovers only on STALL. xHCI 4.10.2.1 (endpoint
halted when CErr is exhausted), 4.8.3 (doorbells ignored while Halted),
4.6.8 and 4.6.10. The 20 s banner delay fits two or three 5 s data
timeouts plus pauses; "two READ failures then 2 MiB fine" fits a CBW
(OUT) failure, which is recovered; "every later `update` too" fits an IN
failure, which is not.

Instrument: on every `bulk()` error including 0 print stage, direction,
DCI, completion code, event TRB pointer versus the last TRB, residual,
time, and the Endpoint Context state (word 0 bits 2:0) and TR Dequeue
Pointer of DCI 1 and both bulk DCIs from the Output Device Context (keep
its virtual address in `UsbDevice`; it is discarded today).

Fix: treat every halting code (2, 3, 4, 5, 6, split errors) on the
endpoint that reported it identically and at every stage: Reset Endpoint
(check its code), Set TR Dequeue (check its code); `CLEAR_FEATURE(HALT)`
only for STALL. For code 4 the specification's own mechanism is Reset
Endpoint with TSP = 1 (Soft Retry, 4.6.8.1) plus a doorbell; if Panther
Point does not honour it, Reset Endpoint TSP = 0 + `CLEAR_FEATURE(HALT)`
+ Set TR Dequeue past the failed TD, then BOT Reset Recovery.

## H-B (high confidence): `recover_bulk` resets endpoints that are not halted and then desynchronises the toggle

`reset_recovery` calls `recover_bulk` on both endpoints; at most one is
halted, so Reset Endpoint goes to a Running endpoint, fails with Context
State Error (19), nobody looks (`let _ =`), and `CLEAR_FEATURE(HALT)`
resets the device toggle to DATA0 while the host's stays (USB 2.0 9.4.5;
BOT 3.1 preserves toggles across the class reset). At High Speed half of
the next CBWs are ACKed and discarded: a 5 s data timeout, then the
toggles realign. Fix: Reset Endpoint only when the Output Endpoint Context
says Halted; for the other endpoint use Stop Endpoint then Configure
Endpoint Drop + Add (xHCI 4.6.8 note, Linux `xhci_endpoint_reset`); fail
loudly on any non-SUCCESS command completion.

## H-C (high confidence as the "stopped answering" explanation): port events are discarded, so a device that drops or re-enumerates leaves the slot addressing nothing

`poll()` clears all change bits without reading them; `stash_event` drops
Port Status Change events during transfers; the Bulk-Only reset result is
ignored; there is no port reset, Reset Device or Address Device path. A
device that reboots on the class reset while programming flash is back at
address 0: every TD gets code 4 forever, no STALL, no timeout, which is
what the second boot showed. Instrument: log every PSC event with port and
PORTSC (CCS, PED, PLS, speed, change bits) before clearing; log PORTSC of
the storage port on every transport failure and after each recovery. Fix:
on CSC/PRC/WRC on the storage port mark the storage unusable and
re-enumerate (Disable Slot or Reset Device, Enable Slot, Address Device,
SET_CONFIGURATION, Configure Endpoint, INQUIRY/TUR/READ CAPACITY); make
port reset + re-enumeration the last resort.

## H-D (medium): timed-out TDs are never retired; stale events are attributed to later transfers

`bulk()` returns on timeout leaving the TRBs owned by the controller; the
next transfer enqueues behind them; any non-success event on the DCI is
taken as the current transfer's result without comparing the pointer;
`serve_block` closes the client buffer handle right after, so a late IN
completion writes into memory that may be reused. Fix: on timeout Stop
Endpoint, consume the Stopped event, Set TR Dequeue to the enqueue
pointer, and only then return and release the buffer; match error events
by TRB pointer within the current TD.

## H-E (medium): a single error on EP0 halts the control pipe forever

`control()` examines only the event whose pointer equals the Status Stage
TRB; an error event points at the Setup or Data stage and is discarded;
after 1 s it returns None; no Reset Endpoint on DCI 1 exists. Reachable
from H-A's first error, since a device that does not answer the CBW will
not answer the SETUP of `CLEAR_FEATURE` either; afterwards every recovery
silently fails. Fix: on a control error Reset Endpoint DCI 1 + Set TR
Dequeue past the three stages; treat `control()` failures in recovery as
failures.

## H-F (low, hygiene): Link TRB never carries the Chain bit

Benign here (chunks end on a 64 KiB boundary); fix anyway.

## Verified correct

`ProducerRing::push`, `initial_link`, `EventRing::accept`, `bulk_trbs`
(no 64 KiB crossing, TD Size, CHAIN/IOC/ISP), endpoint contexts (types,
MPS, CErr 3), Reset Endpoint and Set TR Dequeue encodings, contiguous
client buffers, Panther Point quirks irrelevant to code 4, IMAN handling
with MSI.

## Decision table for the laptop log

| Observation | Confirms |
|---|---|
| After a code 4 on DATA-IN/CSW: EP state 2 (Halted) and the next failure is code 0 exactly 5000/1000 ms later | H-A |
| Reset Endpoint or Set TR Dequeue completion code 19 inside the recovery | H-B |
| PSC event with CSC/PRC, or CCS = 0 / PLS not U0, right after the BOT reset; code 4 on every TD afterwards | H-C |
| Transfer Event with a pointer outside the current TD, or controller dequeue behind software enqueue at a failure | H-D |
| Transfer Event on endpoint 1 with a code other than 1/13; `control()` returning None afterwards | H-E |
| First failure with EP state Running, PORTSC clean, code 4 on the CBW stage, right after a write | the trigger is the device; only correct recovery helps |

Also print the port speed at `configure_storage` (3 = High Speed
expected) and USBSTS at each failure (HSE would mean a host DMA fault).

## Proposed fixes, in order

1. Keep the Output Device Context pointer; read the EP state before any
   Reset Endpoint; check every command completion; recover the halted
   endpoint at every stage; `CLEAR_FEATURE` only on STALL; Soft Retry or
   TSP = 0 + `CLEAR_FEATURE` for transaction errors (H-A, H-B, H-E).
2. Implement Stop Endpoint; use it on timeouts and, with Configure
   Endpoint Drop + Add, to reset the toggle of the non-halted endpoint
   during BOT Reset Recovery (H-B, H-D).
3. Match Transfer Events by TRB pointer in `bulk()` and log the rest (H-D).
4. Log PSC events, detect disconnect or re-enumeration of the storage
   port, port reset + re-enumeration as the real last resort (H-C).
5. Chain bit on the Link TRB for spanning TDs (H-F).
