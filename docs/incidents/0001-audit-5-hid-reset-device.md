# Incident 0001, audit 5: HID endpoint recovery and Reset Device

Branch `fix/usb-hid-and-reset-device` (commits ee2ba6d, 28d28e2).

Verdict: not safe to merge as it stands; safe once M1, M2, M3 and the QEMU runs (H1) are done. Checked and correct: Reset Device only in Addressed/Configured after a port reset (4.6.11); Address Device after Reset Device with BSR=0, fully rewritten Input Context (A0|A1), emptied EP0 ring with DCS=1 (4.6.5; Linux copies the enqueue pointer instead, both conform); Configure Endpoint re-adds bulk endpoints; spare Output Device Context zeroed (4.3.3) and DCBAA rewritten, reuse gated on no outstanding Disable Slot; HID re-arm without double queueing; no nesting (maintain_hid only from the main loop); ordering Reset Endpoint TSP=0 → Set TR Dequeue → CLEAR_FEATURE → doorbell (4.6.8, USB 2.0 9.4.5); deadlines never started expired; held-buffer bookkeeping intact. H1 scenarios never run (usb.hid_fault, strict.usb_hid_halt, usb.reset_device plus existing usb/strict/hid/keyboard scenarios). M1 spare memory reused after a failed Disable Slot (4.6.4: released only on successful completion). M2 completion codes lost in enumeration. M3 no backoff between HID recovery attempts (3 attempts in ~30 ms, keyboard lost for the boot). M4 usb.reset_device does not prove "no DMA memory allocated". M5 dropped key events in dispatch(ev, None), new waits add places where it happens. M6 latency comment wrong (77 s bound still under 90 s). M7 no rate limit on HID fault records. L1 SlotReuse::Readdress unreachable. L2 silent drop when a spare is held. L3 error stray events not recorded. L4 Panther Point strictness stated as fact. L5 spurious Success after Short Packet on Panther Point (F15) prints stray events on hardware.

## Re-audit

Verdict: safe to merge once the QEMU scenarios pass. The four remaining
items, fixed on the branch:

1. A second failure during `maintain_hid` could be lost (the attempt
   cleared the fault and re-queued reports on a possibly Halted endpoint).
   Fixed: `Endpoint::failed` sets `refault` when a fault is already
   pending; each attempt clears it when it starts and, if it is set again
   by the end, keeps the fault and recovers again from the endpoint state
   after the backoff instead of re-queueing.
2. `Pending::push` dropped the oldest event silently when full. Fixed:
   drops are counted, the first one is printed and recorded (`usb hid evt
   dropped`), and the running total is printed when the client next takes
   events.
3. One stray error was recorded twice (`usb stray err` and `usb hid err`).
   Fixed: a stray error on a HID endpoint goes only through
   `Endpoint::failed` (`usb hid err`); `stray_transfer` prints and records
   the others.
4. A given-up endpoint could be re-armed by later stray errors (a second
   `usb hid lost`). Fixed: `Endpoint::given_up`; later failures are only
   counted.
