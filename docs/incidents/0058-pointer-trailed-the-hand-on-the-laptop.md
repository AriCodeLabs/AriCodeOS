# Incident 0058: The pointer trailed the hand on the laptop

- Opened: 2026-11-04. Status: fixed on `fix/compositor-input-latency`.
- Found by: the owner, on the reference laptop running release/0.18.0
  (ca3a558) in the desktop. The touchpad (PS/2) moved the pointer, but
  "not smoothly: when I change direction it takes a while to respond".

## Evidence

- On the laptop the compositor reads four input services: `usb_input`
  (xHCI), two `usb_ehci` and `ps2_input` (`diag`). Only the PS/2 one
  moves; the other three are idle.
- QEMU, a stroke of 150 moves, one every 10 ms (a touchpad reports about
  100 times a second; `mouse_steps` in `[host_input]`, new):
  - through the PS/2 mouse with one idle EHCI beside it: no lag;
  - through the USB mouse with an idle EHCI and the idle PS/2 pointer
    beside it (three devices): the compositor handled events more than
    50 ms after their arrival (`gui input late x1`, then `x4`, the new
    record below).

## Mechanism

- The compositor's loop read each device in turn with a 1 ms timeout
  (`InputClient::read(1, ..)`), then composed.
- The kernel's timer runs at 100 Hz: a 1 ms wait on an idle device lasts
  until the next tick, up to 10 ms. Every idle device added that to each
  round.
- A reply carries at most 4 events (`input::MAX_EVENTS`), and one PS/2
  packet is 3 of them (X, Y, `EV_SYN`). One round handled about one
  packet.
- With three idle devices a round took up to 30 ms, so the compositor took
  in about 40 packets a second while the touchpad sent 100. The rest
  queued in the driver (128 events), so the pointer showed where the
  finger had been: a change of direction appeared late. Once the queue
  was full, the oldest moves were dropped.

## Fix

1. **One wait for everything.** The compositor keeps one read outstanding
   on every device (`InputClient::start_read`, ADR 0044) and sleeps in a
   single `ipc_receive_any` on its endpoints and the devices' reply
   endpoints. It wakes for a request, for a device's events, or at its
   next deadline (frame, poll, sweep), whichever comes first. An idle
   device costs nothing.
2. **Every reply that is there.** After each wake, every outstanding read
   is looked at without waiting. The events are handled in time order, as
   before, and a driver that died holding a read is noticed even while
   another device keeps sending (as in the console bridge).
3. **Late input is recorded.** An event handled more than 50 ms
   (`LATE_MS`) after its driver received it is counted. The count goes
   into the kernel event ring as `gui input late xN` at most once a
   second, so `diag` shows it on a real machine.

## Tests

- `gui.pointer_stroke` (new): the USB-mouse stroke with three devices
  idle. Before the fix it failed with `gui input late`; now it passes.
  The click lands at the point the moves add up to, so no move was lost.
- `gui.ps2_pointer_stroke` (new): the same stroke on the PS/2 mouse.
- The `gui` and `input` areas: 33 of 33.
- The laptop: see below.

## On the laptop

To be filled in with the owner's test of the dev build.
