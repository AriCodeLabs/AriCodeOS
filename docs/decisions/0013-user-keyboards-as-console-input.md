# ADR 0013: User-space keyboards as console input (console bridge)

- **Date:** 2026-09-28
- **Status:** Accepted

## Context

The console belongs to the kernel: COM1 and the PS/2 keyboard arrive by
interrupt into a ring that `console_read` (syscall 24) hands to whoever
holds `READ` on the `Console` object. The virtio-input and USB HID
keyboards live in ring 3 (`inputd`, `usbd`) and are read only by the
compositor and by programs that receive their endpoint. On a machine with
no serial port and no PS/2 (a laptop with an internal USB keyboard,
Phase 34) the shell had no keyboard. Stage 2 of the post-roadmap plan asks
for a usable session with those keyboards.

## Decision

1. **The console stays in the kernel.** The shell reads a single ring
   without knowing where each byte comes from; COM1 and PS/2 keep working
   without user processes (panics, early boot, console tests).
2. **New syscall `console_inject` (29)**: writes text into the console
   input ring. It requires `MANAGE` on a `Console` object; `READ` (what the
   shell and programs receive) is not enough. Only init receives the
   handle with `MANAGE`, and it delegates it to whoever it chooses.
3. **A `console_bridge` service** (`services/console-bridge`) receives the
   console input with `MANAGE` and the endpoints of the input services,
   translates key presses with `aricode_input::Keyboard` (one state per
   keyboard; mouse buttons are ignored) and injects them. Init launches it
   as a supervised resident (ADR 0011, driver policy) in `shell` mode, only
   when there is no compositor: the input services accept one waiting
   reader, and with a GUI the events belong to the compositor.
4. **One reader per input service.** While the bridge runs, a program
   launched from the shell that asks for input competes with it and will
   see `Busy` or lose key presses. This is the stage 2 contract (shell
   session); the graphical session does not launch the bridge.

## Rejected alternatives

- Moving the console to user space (a `consoled` with COM1 and PS/2): adds
  a process to the panic and early-boot path and forces handing over the
  serial line, with no gain for the shell.
- Having the shell read the input endpoints itself: duplicates the
  translation in every console reader (shell, test programs) and ties the
  shell to the input services; with the bridge the shell keeps reading a
  `Console` with `READ`.
- An input focus system (routing events between bridge, compositor and
  applications): that is stage 4 (external applications), not stage 2.

## Consequences

- Init receives `0x10008` with `READ|MANAGE|DUPLICATE|TRANSFER` and still
  passes only `READ|TRANSFER` to the shell.
- The bridge polls the services in turn with a `POLL_MS` (20 ms) timeout:
  the worst-case latency of a key press grows with the number of devices.
- The bridge writes nothing at startup: init announces its launch
  (`CONSOLE_BRIDGE_READY`) before creating the shell, because with several
  CPUs a line from the bridge got interleaved with the first prompt.
- `shell.usb_keyboard` and `shell.virtio_keyboard` test a shell session
  typed via QMP on a USB HID keyboard and on a virtio one;
  `shell.services_detail` shows the bridge as a resident.
