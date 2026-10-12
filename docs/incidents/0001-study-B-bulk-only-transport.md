# Incident 0001, study B: Bulk-Only Transport and SCSI layer

Read-only review of `services/usb-input/src/main.rs`,
`libs/usb-storage/src/lib.rs` and `libs/xhci/src/lib.rs` against the USB
Mass Storage Bulk-Only Transport 1.0 specification, xHCI 1.x and Linux
`usb-storage`. The encode/decode layer (`Cbw`, `Csw`, CDBs, sense) is
correct; the problems are in the transport state machine.

## H1 (most likely): the device stops answering at bus level and the driver cannot see it

Completion code 4 means no valid handshake three times in a row (CErr = 3,
`EndpointContext::bulk`): a busy stick NAKs, it does not produce code 4.
A stick that reboots on the Bulk-Only Mass Storage Reset disconnects and
re-enumerates; the slot the driver keeps using no longer exists, so every
TRB ends in code 4 forever, which matches the persistent failure after the
reset. The driver discards Port Status Change events (`stash_event`,
`poll` only clear the change bits), never reads PORTSC after enumeration,
and throws the device descriptor away (`let _ = desc;`): VID:PID, bcdUSB,
bMaxPower and the negotiated speed (PSIV) are never logged.

Linux order on a transaction error: port reset of the device (Reset
Device, re-address, SET_CONFIGURATION, descriptors compared), retry the
SCSI command, and only if that fails the Bulk-Only reset. Never
pause-and-retry at the transport level.

Instrument: on every failed `bulk()` print phase (CBW/data/CSW),
direction, code, residual, elapsed ms and PORTSC decoded (CCS, PED, OCA,
PR, PLS, PP, speed, CSC, PEC, OCC, PRC, PLC, CEC); log Port Status Change
events; log VID:PID, bcdUSB, bMaxPower, PSIV at enumeration. Decision:
CSC = 1 or PED = 0 or PLS != 0 at the failure means the device dropped or
reset itself; stable port with code 4 on the CBW means alive but not
handshaking (power or signal; try another port and another stick).

Fix if confirmed: `port_reset_storage()` (PORTSC reset, wait PED, Reset
Device, Address Device, SET_CONFIGURATION, Configure Endpoint with fresh
bulk rings, keep capacity and interface) as the first response to a
transaction error and to a port change with CCS = 1; with CCS = 0 mark the
storage gone and fail requests at once.

## H2: after code 4 the endpoint is Halted; the driver only recovers STALL

xHCI 4.10.2.1: on a USB Transaction Error the endpoint transitions to
Halted once CErr is exhausted; Reset Endpoint then Set TR Dequeue are
required. In `scsi()` a CBW failure recovers for any code, but the data
phase and the CSW only recover on `completion::STALL`; code 4 on data-IN
or CSW leaves bulk-IN halted, the next CBW goes out on the healthy OUT
endpoint and its data phase times out at 5 s (code 0) on the halted ring.
Linux (`xhci_requires_manual_halt_cleanup`) treats transaction, babble and
split errors like a STALL.

Instrument: keep the output device context address (discarded at
`let (dev_ctx_phys, _) = ...`); print EP context state (0 Disabled,
1 Running, 2 Halted, 3 Stopped, 4 Error) of the failed DCI.

Fix: recover the host side for any non-success code: Reset Endpoint + Set
TR Dequeue + CLEAR_FEATURE(HALT) on the failed endpoint; then H3.

## H3: a new CBW is sent while the previous command is outstanding (BOT violation)

BOT 5.3 and 6.7: after a CBW is accepted, the transport returns to Ready
only through a CSW or a Reset Recovery. A data or CSW failure returns from
`scsi()` without reading the CSW and without Reset Recovery; `block_io`
sleeps and sends a new CBW with tag + 1. The device may deliver the old
CSW (tag mismatch, counted as another transport failure), stall both
pipes, or silently restart (what QEMU does, which is why QEMU never
reproduces this). Moving the Reset Recovery to the sixth failure makes
this worse.

Instrument: on tag mismatch print both tags and the CSW status/residue.

Fix: after a data-phase error (and host-side halt recovery) read the CSW
once with a short timeout; if missing or invalid, Reset Recovery or port
reset before any new CBW. Never retry READ/WRITE by just sending another
CBW.

## H4: Reset Recovery differs from the specification and Linux

(a) No delay after the class reset: Linux waits 6 s before clearing the
halts; CLEAR_FEATURE sent to a device still resetting fails with
transaction errors on EP0. (b) EP0 is never recovered: a STALL or
transaction error on EP0 halts it; `control()` returns None and every
later control transfer times out at 1 s. (c) Data toggle hazard: Reset
Endpoint on a Running endpoint fails with Context State Error, Set TR
Dequeue fails, but CLEAR_FEATURE(HALT) still resets the device toggle to
DATA0 while the host stays at DATA1: single-packet CBWs/CSWs are ACKed
and discarded every other time. xHCI 4.6.8: to reset the toggle of a
non-halted endpoint use Stop Endpoint then Configure Endpoint with Drop +
Add. All completion codes in `recover_bulk` and `reset_recovery` are
ignored (`let _ =`).

Instrument: print the completion codes of Reset Endpoint and Set TR
Dequeue, the result of each `control()` in recovery, and the EP0 state
after a control failure.

Fix: check the codes; Drop + Add Configure Endpoint (or rebuild both bulk
endpoints with fresh rings) on Context State Error; recover EP0 after any
failed control transfer; wait after the Bulk-Only reset (poll GET_STATUS
on EP0 every 200 ms up to 6 s); fall through to the port reset.

## H5: timeouts far shorter than Linux; timed-out TRBs never cancelled

CBW 1000 ms, data 5000 ms, CSW 1000 ms, against Linux's 30 s SCSI command
timeout. A stick that programs NAND asynchronously can NAK the CSW of a
READ right after a 2.3 MB write for seconds. On a timeout the TRB stays on
the ring with no Stop Endpoint + Set TR Dequeue; `serve_block` closes the
client's DMA handle right after, so a late completion writes into memory
that may be reused (no IOMMU on the laptop). `bulk()` attributes any error
event on the endpoint to the current transfer regardless of
`ev.parameter`.

Fix: raise the timeouts; on timeout Stop Endpoint then Set TR Dequeue
before returning; accept only error events whose TRB pointer lies in the
current TD.

## H6 (checked, nothing wrong): speed, LPM, suspend, packet sizes

No LPM, no U3; max packet sizes come from the descriptors; EP0 is fixed
with Evaluate Context; slot speed from PSIV. Panther Point quirks in Linux
do not produce code 4. Linux has no "delay after write" flag; its
robustness is the 30 s timeout plus the port reset on error.

## Instrumentation plan (one build)

1. `Storage` keeps `port` and the device context address; decoded PORTSC
   and EP state with phase, code, residual and elapsed on every failed
   transfer.
2. Port Status Change events logged with port and PORTSC.
3. Completion codes of Reset Endpoint / Set TR Dequeue and each
   `control()` result in recovery.
4. Both tags on a CSW mismatch.
5. VID:PID, bcdUSB, bMaxPower, PSIV at enumeration.
6. Reproduce with a small `write` then `cat`, and an `update` (2.3 MB)
   then a read.
