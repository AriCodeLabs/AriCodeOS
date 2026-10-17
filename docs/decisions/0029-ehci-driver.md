# ADR 0029: User-space EHCI driver with USB 2.0 hubs

- **Date:** 2026-10-17
- **Status:** Accepted (written and host-tested; QEMU scenarios written,
  not yet run; not yet exercised on the laptop)

## Context

The reference laptop (Intel 7 series, `docs/hardware.md`) has two EHCI
controllers, `00:1a.0` (`8086:1e2d`) and `00:1d.0` (`8086:1e26`), next to
the xHCI (`8086:1e31`) that serves the boot stick (ADR 0023). Until now
init printed "has no driver" for them, so every device on a port that the
xHCI does not take was invisible: on Intel 7 series the xHCI can only take
the ports the firmware marks as switchable in `USB2PRM` (usbd copies that
mask into `XUSB2PR`, ADR 0023 point 5); every other port, external or
internal, stays on an EHCI.

Three facts shape the driver:

1. Intel 6/7 series put an integrated **rate matching hub** (a USB 2.0 hub
   with a transaction translator) behind each EHCI root port instead of
   UHCI companion controllers (`N_CC = 0`). Keyboards and mice are low or
   full speed, so they are reached through split transactions on that hub
   (EHCI 4.12), never directly.
2. Intel EHCI functions have **neither MSI nor MSI-X**, and INTx is not
   supported by the kernel (ADR 0022). QEMU's `usb-ehci` has no MSI either.
3. The failure protocol (ADR 0028) applies: every failure visible, bounded
   retries only where a specification asks for them, `strict` halts.

## Decision

1. **A separate driver, `usb_ehci` (`services/usb-ehci`).** A controller has
   one owner; EHCI and xHCI share nothing at the register level, so the
   EHCI driver is its own process with the same contract as usbd (`SERVICE`
   input endpoint, `SYSTEM`, block endpoint) and the same protocols
   (`InputServiceProtocol v1`, `aricode-proto::block`).
2. **Pure parts in `libs/ehci` (`aricode-ehci`)**, host-tested: register
   offsets and bits (EHCI 2.2, 2.3), PORTSC writes that never echo the
   write-1-to-clear bits (2.3.9), the USBLEGSUP walk and SMI disabling
   (2.1.7, 5.1), qTDs and queue heads in the 32-bit and 64-bit (Appendix B)
   layouts, transfers split on packet boundaries (4.10.6), completion
   decoding (4.10.3, 4.15.1.1), interrupt schedule masks (4.12.2, USB 2.0
   5.7.4), transaction translator selection (USB 2.0 11.14) and the hub
   class: descriptor, port status, requests (USB 2.0 11.23, 11.24). The
   USB 2.0 chapter 9 structures (SETUP packets, descriptors, configuration
   parsing) are reused from `aricode-xhci`, and the bulk-only/SCSI layer
   from `aricode-usb-storage`.
3. **Polling, no IRQ.** `sdk::virtio_pci` learns the handle layout without
   an IRQ (`polled_bar_handle`; `extra_handle` is corrected for it). The
   driver disables controller interrupts (`USBINTR = 0`) and polls: the
   main loop waits up to 8 ms for a request, then looks at the HID qTDs,
   the root ports, the hub ports (every second, GetPortStatus) and at most
   one pending enumeration. A transfer is polled with yields for 2 ms, then
   with 1 ms sleeps, up to its timeout.
4. **Controller bring-up (EHCI 4.1, 5.1).** Firmware handoff through
   USBLEGSUP (1 s for the firmware, as Linux; a firmware that keeps the
   controller is reported and recorded, and halts the driver under
   `strict`), stop, HCRESET, a 1024-entry frame list, an async schedule
   head that is never executed (H bit, halted overlay), both schedules on,
   `CONFIGFLAG = 1`, port power when PPC is set. On a 64-bit controller
   every structure sits in one 4 GiB segment (`CTRLDSSEGMENT`) and data
   buffers use the extended pointers; a 32-bit controller refuses buffers
   above 4 GiB with a reported error.
5. **Queues.** One queue head per endpoint from a fixed pool of 24, each
   with four qTDs and an inactive "stop" qTD that a short IN packet jumps
   to (alternate pointer, 4.10.2; the control data stage jumps to its status
   stage instead). Work is handed to an idle queue head by clearing its
   overlay pointers, writing the qTDs (token last), making the overlay token
   idle with the toggle kept, and only then setting the next pointer, so the
   controller can never reach a half-written or stale qTD. Async queue heads
   leave the schedule with the Interrupt on Async Advance doorbell (4.8.2)
   before they are changed or reused; interrupt queue heads form one chain
   that every frame list entry at the chain's shortest period points to,
   and leave it two frames before reuse (4.6).
6. **Enumeration.** Root ports: low-speed line state or no enable after
   the reset means a full- or low-speed device, handed to a companion
   controller if there is one, otherwise reported (4.2.2). Hubs: configured,
   ports powered, `bPwrOn2PwrGood` plus the 100 ms debounce, then every port
   is looked at; hub ports are reset with `PORT_RESET` and acknowledged with
   `C_PORT_*`. Devices: 8 bytes of the device descriptor, `SET_ADDRESS`
   (2 ms recovery), EP0 queue head rewritten out of the schedule, full
   descriptors, then hub, boot HID (SET_PROTOCOL, SET_IDLE; HID 1.11) or
   bulk-only storage (INQUIRY, TEST UNIT READY, READ CAPACITY(10)), at most
   one storage device. A device that fails its enumeration has its port
   disabled. Five hub tiers at most (USB 2.0 4.1.1). Connection changes
   detach a device and everything below it, and enumerate a new one after
   the debounce.
7. **Failures (ADR 0028).** Every failed transfer is printed with the qTD
   token and recorded (`ehci <kind> <error>`: `stall`, `babble`, `databuf`,
   `xact`, `missed`, `timeout`, `buffer`, `lost`); block requests also
   record `usb r|w lba N` with the error code. The only transaction retries
   are the controller's three tries (CERR, 4.15.1.1.1). A STALL is cleared
   with `CLEAR_FEATURE(ENDPOINT_HALT)` (USB 2.0 9.4.5) once per stall;
   storage then follows the Bulk-Only rules (BOT 5.3, 6.7: one CSW read
   after a DATA/CSW failure, Reset Recovery after a CBW failure or an
   invalid CSW, BOT 5.3.4) and the request answers `Io`: the command is not
   resent. A failed Reset Recovery leaves the storage unusable until it is
   plugged in again; a HID endpoint that fails beyond a cleared STALL stops
   until then. A transfer error seen while the device's port reports it
   unplugged is the disconnect, reported by the port change. A queue head
   the controller does not release is never reused, nor the client buffer
   it pointed to. A unit attention is retried once, only while the block
   client's 90 s wait still covers the worst case of the retry (64 s); the
   SCSI windows are 30 s for medium commands and 5 s for commands that
   touch no medium. With `strict` the first failure dumps USBCMD, USBSTS,
   FRINDEX, every PORTSC and every queue head with its qTD tokens before
   any recovery, and the driver exits with code 15; init restarts nothing.
   A host system error (4.15.2.4) dumps and exits with code 16.
8. **Init.** For class `0c.03` init launches usbd for interface `0x30`
   first and then `usb_ehci` for interface `0x20`; other interfaces (UHCI,
   OHCI) still get "has no driver". Every USB driver gets a block endpoint;
   without virtio-blk the first one that reports a capacity becomes the
   block service. `usb_ehci` serves its block endpoint only after its boot
   enumeration (every port seen at start, including hub ports) is done, so
   init's single capacity question waits for it.
9. **Port routing is left alone.** The EHCI driver never touches `XUSB2PR`
   or `USB3_PSSEN` (they live in the xHCI's configuration space); usbd
   keeps routing the switchable ports to the xHCI. Because usbd runs first
   and only reports ready after its own enumeration, the routing is done
   before the EHCI driver looks at its ports.

## How a port ends up on an EHCI (Intel 7 series)

- Each USB 2.0 port pin is wired to the EHCI's rate matching hub and, for
  the switchable ones, also to the xHCI. `XUSB2PR` bit n = 1 sends port n
  to the xHCI; 0 leaves it on the EHCI. The firmware publishes which ports
  may switch in `USB2PRM`.
- usbd writes `XUSB2PR = USB2PRM`: every switchable port goes to the xHCI.
  A port ends up on the EHCI when its `USB2PRM` bit is 0 (not switchable:
  typically the USB 2.0-only ports and internal devices such as a webcam,
  Bluetooth or a card reader), when no xHCI driver ran (usbd failed before
  routing), or on chipsets without an xHCI.
- On the EHCI, the device appears behind the rate matching hub on root port
  1 (`usb-ehci: port 1.N: ...`); `diag` and the console show the path.

## Consequences

- Devices on EHCI-only ports work: USB 2.0 sticks as the system disk when
  no stick is on the xHCI, keyboards and mice through the rate matching
  hub. Unsupported classes (video, Bluetooth, card readers) are enumerated
  and left unconfigured with a line on the console.
- Polling costs a wake-up every 8 ms per EHCI; HID reports are seen within
  about one poll period plus the endpoint's interval.
- Hubs and split transactions cannot be tested in QEMU (its hub is full
  speed and cannot hang off an EHCI without companions); they are covered
  by host tests of the layouts and wait for the laptop.
- The xHCI driver keeps its own copies of the input event ring and the
  fault injection; sharing them is left for a later cleanup of both drivers.
- QEMU scenarios (`usb.ehci_hid`, `usb.ehci_storage`, `usb.ehci_fault`,
  `strict.ehci_halt`, and the revised `usb.hid_boot_with_ehci`) were written
  with this ADR but not run when it was accepted.
