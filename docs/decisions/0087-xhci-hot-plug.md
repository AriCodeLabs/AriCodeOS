# ADR 0087: Plug and play on the xHCI root ports

- **Date:** 2026-11-11
- **Status:** Proposed (branch `feature/usb-hotplug`); the points under "Left
  for the owner" wait for a decision.

## Context

usbd (ADR 0023) enumerated the xHCI root ports once, at boot. A Port Status
Change Event only printed PORTSC and re-enumerated the storage device's own
port; a mouse plugged in later was never seen, and one unplugged kept its slot
and any key or button it held. On the owner's office laptop (Tiger Lake, xHCI
`8086:a0ed`) that meant a reboot for every mouse. The study
(`docs/audits/2026-11-11-xhci-hot-plug-study.md`) read the xHCI and USB
sections involved and how Linux, FreeBSD and Haiku do it and where they
failed.

## Decision

1. **Events, not polling.** Port Status Change Events (xHCI 4.19.2) drive
   everything; every change bit is cleared on each event so the next change
   raises one. The classification is pure and host-tested
   (`aricode_xhci::hotplug::port_action`): a connect change, or a port
   disabled under a device (PEC with PED clear), means the device the driver
   had there is gone; a connection present after such a change is debounced.
2. **Event-driven debounce.** A connection is enumerated after 100 ms
   without another change (USB 2.0 7.1.7.3; `DEBOUNCE_MS`), each change
   restarting the interval (`PendingPorts`), from the idle loop, one bounded
   step per iteration, like the boot enumeration. A failure is retried with
   a port reset up to `ATTACH_ATTEMPTS` (3), unless the refusal is final.
3. **A connect change is a new device.** Unlike Linux's "resuscitation", the
   driver does not try to keep a device whose descriptors read the same: a
   boot HID loses nothing by being enumerated again.
4. **Detach.** The held keys and buttons are released at once (`release_all`,
   shared with the EHCI driver); the device's reports are no longer decoded,
   queued or recovered, and its failed transfers are the unplug, not faults.
   The idle loop then stops its running endpoints (xHCI 4.6.9), disables the
   slot (4.6.4), and its memory (contexts, EP0 ring, HID rings and report
   buffers) joins a pool of `MAX_DEVICES` + 1 spares, only once the Disable
   Slot released the slot. Plugging and unplugging does not grow the DMA
   arena.
5. **USB3 links** in SS.Inactive or Compliance Mode, or with Cold Attach
   Status, get a Warm Port Reset (`port_reset`), as Linux and FreeBSD do.
6. **Refusals on the console.** A fifth device (`MAX_DEVICES` stays 4), an
   unsupported device, storage plugged in after boot and a second storage
   device are refused with a line and not retried.
7. **Storage keeps its own logic**: re-enumerated on its own port as before.
8. **One policy, as parameters.** Debounce (100 ms), reset recovery (50 ms:
   USB 2.0 TRSTRCY is 10 ms, Linux waits 50), attempts (3), the retry
   pause (200 ms times the failed attempts) and port resets per attempt (5,
   Linux `PORT_RESET_TRIES`) are one `aricode_xhci::hotplug::Policy` the
   driver passes in. The boot key
   `usb.hotplug=debounce:MS,recovery:MS,attempts:N,retry:MS,resets:N` (any
   of them) tunes it within bounds the specifications set (a debounce of at
   least 100 ms, a recovery of at least 10 ms); a refused value is said and
   the defaults stay.
9. **Disable Slot commands never overlap.** One is outstanding at a time;
   any other, whether from the idle loop or from a block request giving up
   the storage's endpoint, waits in line (`waiting_disables`) and is issued
   when the one before completes. One with no completion in 5 s (Linux's
   command timeout) is given up: its slot's memory and held buffers are
   kept for good, never reused, and the line moves on.
10. **Resets and enumerations stand only on a settled connection.** A reset
    ends when PR is clear and the port enabled and connected; a USB2
    connection that changes during the reset or its recovery is reset again
    (up to 5 times, as Linux), a USB3 one that bounces across a warm reset
    is not. An enumeration only stands if the device it addressed is still
    the one on its port (`still_attached`); otherwise the port is debounced
    again, at most `attempts` times in a row before it is left alone until
    it is empty (`PortBounces`).

## Left for the owner

- USB storage plugged in after boot: init chooses its disks at boot; serving
  later disks needs init to watch the block services.
- External hubs on the xHCI: the work is listed in the study (hub class
  requests, shared with `libs/ehci::hub`; Route String and TT fields; a
  larger `MAX_DEVICES`).
- A keyboard plugged in after boot does not start the desktop session that
  init skipped for want of one.

## Consequences

- QEMU scenario `usb.xhci_hotplug` (release level) covers plug, unplug,
  re-plug on the same and another port, slot and memory reuse, a held button
  released on unplug, the fifth-device refusal, and a hot-plugged keyboard
  typing into the shell. `usb.xhci_hotplug_storage` does it next to the boot stick
  while it serves writes, with the stick counted among the four devices and
  a second stick refused. The runner's `[[host_hotplug]]` gained unplug
  stages, each after the guest's answer to the previous one.
- Real hardware has not run it yet: the office laptop's trace lines are in
  the study.
