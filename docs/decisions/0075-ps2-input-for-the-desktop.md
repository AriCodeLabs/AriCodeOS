# ADR 0075: The PS/2 keyboard and touchpad in the desktop

- **Date:** 2026-11-04
- **Status:** Accepted. The owner, 2026-11-04, asked to continue with
  everything after phase B of ADR 0074. Giving the desktop the PS/2
  keyboard and touchpad was the open item phase B left them.

## Context

A laptop's internal keyboard and touchpad sit behind the i8042 PS/2
controller. The kernel owns that controller:
- it is the console's keyboard (Phase 34), before any service runs and
  after the desktop ends;
- since ADR 0074 phase B, while a process holds the screen the kernel gives
  the console none of its keys and acts only on Ctrl+Alt+Backspace;
- the aux port, where the touchpad is, has never been enabled.

The compositor reads only input services (virtio-input, usb-input,
usb-ehci). On a laptop without a USB keyboard the desktop therefore cannot
be typed at, and init refuses to start it (ADR 0074, audit M2).

### How veterans split it

- **Redox (`ps2d`), Fuchsia (i8042 driver) and Genode (`ps2_drv`)** run the
  whole controller in user space. The kernel grants port I/O (Redox's
  `iopl`, Genode's I/O port sessions) and the IRQ lines. None of their
  kernels has a keyboard console of its own: early and panic output goes to
  serial.
- **Linux and Haiku** keep the controller in the kernel. Linux's `i8042`
  moves bytes and `psmouse`/`atkbd` decode them, all in ring 0. Their
  well-known failures are in the decoders: lost packet sync on touchpads,
  and stuck keys after missed releases. They are not in the byte pump.

Our kernel needs the controller exactly where the user-space designs give it
away:
- the console before init;
- the Ctrl+Alt+Backspace safety net while the desktop holds the screen;
- the console after the desktop.

Handing the controller to a driver and taking it back means two owners, one
after the other, of a device with command/acknowledge state. It also needs
port I/O from ring 3, which nothing in AriCodeOS grants today.

## Decision

**The kernel stays the i8042's only owner and only moves bytes. A ring-3
service, `ps2-input`, decodes them and serves InputServiceProtocol v1 like
the other input drivers.**

### 1. The kernel initializes both ports

At boot, after the keyboard, the kernel brings up the aux port:
1. It enables the port and checks that the controller has one: the
   configuration's port-2 clock bit clears after enabling it.
2. It runs the port test. `0xA9` answers 0, but like Linux it also
   accepts `0xFA` and `0xFF`, which many chipsets give on a working port.
   The device's acknowledgement in the next step is what proves a pointer
   is there.
3. It sets the device's defaults (`0xF6`).
4. It tries the IntelliMouse wheel sequence: sample rates 200, 100, 80,
   then reading the ID (3 means a wheel and 4-byte packets).
5. It enables reporting (`0xF4`) and routes IRQ12 to vector `0x42`.

Each device is set up with the other port disabled, so a key or a touch
cannot land where a reply is expected. Commands answered with "resend"
(`0xFE`) are sent again. Both I/O APIC entries are routed before the
controller may raise either line, and the output buffer is drained once
the interrupts are on. An edge on a masked entry would otherwise be lost
and leave both ports silent for the whole boot.

It records what answered:
- the keyboard: acknowledged `0xF4`;
- the pointer: acknowledged its own `0xF4`;
- the wheel: ID 3.

It never parses a mouse packet.

### 2. The bytes follow the screen

- While no process holds the screen, nothing changes. The keyboard types at
  the console, and aux bytes are dropped.
- **While a process holds the screen, every byte goes into one kernel queue,
  tagged with its port** (1 keyboard, 2 aux). The kernel still decodes the
  keyboard for Ctrl+Alt+Backspace and still gives the console nothing.
- When the screen is lent and when it comes back:
  - the queue is emptied;
  - a reset mark (tag 0) is queued first, so each session starts with clean
    decoders and no key left pressed.
- The queue holds 512 entries. When it fills, its last entry becomes a
  reset mark, and the bytes after it are dropped and counted. The reader
  thus releases every key after the entries before the loss: a lost
  release would otherwise leave a key stuck. Entries taken by `ps2_read`
  but not delivered (its buffer unmapped meanwhile) leave a mark too.
- **`ps2=console`** on the command line keeps the phase B behaviour: the
  controller serves only the console, and while the screen is held its keys
  are withheld. This is for someone who wants the internal keyboard kept
  out of the desktop. It is also how the scenarios reach the kernel's
  safety net (§4) without a compositor that hears the chord.

### 3. A capability for the stream

- The stream is a kernel object of a new kind, `PS2` (ABI 8). Init receives
  it as `INIT_PS2`, with `READ | WAIT | DUPLICATE`, only when a controller
  passed its self-test.
- `object_id` on it gives what the kernel found, in `ps2::KIND_*` bits.
- `ps2_read(ps2, ptr, len)` copies the queued entries (two bytes each: tag,
  byte) without waiting. It requires `READ`.
- The object can be a member of `ipc_receive_any`, like an interrupt line
  (`WAIT`): it is ready while entries are queued.
- Init holds it with `READ | WAIT | DUPLICATE | TRANSFER` and hands a
  `READ | WAIT | TRANSFER` duplicate to `ps2-input`. A spawn moves a
  handle only with `TRANSFER`. The kernel lets only one process drain the
  stream: the first to read it, until it is gone. Any other gets
  `Denied`, even one the reader passed the handle to.

### 4. Ctrl+Alt+Backspace: the compositor first, the kernel as the net

The compositor now hears the PS/2 chord through `ps2-input` and leaves by
itself (ADR 0074 phase B), so its clients end cleanly. The kernel used to
stop the holders at once. Now it gives them 2 s to let the screen go, and
stops only one that is still holding. A hung compositor cannot keep the
console from its owner.

### 5. `ps2-input`

- It is an initrd service, started by init at boot when it got `INIT_PS2`,
  and supervised like the other input drivers.
- **Keyboard:** `ScancodeSet1` (`libs/input`), already proven by the
  console.
- **Pointer:** the new `aricode_input::Ps2Mouse`, host-tested:
  - it handles 3- and 4-byte packets;
  - it resynchronizes on bit 3 of the first byte, and after a pause of
    more than 0.5 s between aux bytes, as Linux's `psmouse` does;
  - it drops packets with an overflow bit;
  - it inverts Y and the wheel for evdev;
  - it reports the buttons as `BTN_LEFT/RIGHT/MIDDLE`.
- **On a reset mark** it releases every key and button it saw pressed, so
  the compositor never keeps a modifier down across sessions.
- Its `Info` reply names the device and reports the `KIND_*` bits the kernel
  found. Init's desktop check (a keyboard among the input services) now
  counts the internal keyboard, so a laptop boots into the desktop without
  a USB keyboard.

### Known limit

A pointer that resets itself (it sends `0xAA 0x00`, then reports
nothing) is not set up again until the next boot. Linux reconnects it.
Without suspend and resume, where this mostly happens, it is rare. Doing
it needs commands from the kernel while the ports are live, so it waits
for a case on real hardware.

### What this keeps

- Keys typed at the console never reach `ps2-input`: they flow only while a
  process holds the screen. A compromised `ps2-input` cannot read a password
  typed at the shell.
- Device data is parsed in ring 3. The kernel adds about 150 lines: the aux
  bring-up, a queue and one syscall.

## Tests

- **Host:** `Ps2Mouse` in `libs/input`, covering sync loss, overflow, signs,
  the wheel and buttons.
- **`gui.ps2_two_windows`:** the two-window scenario with only the i8042.
  The PS/2 mouse moves and clicks, and the PS/2 keyboard types into the
  focused window.
- **`gui.ps2_withheld`:** now runs with `ps2=console`. The kernel's net stops
  the compositor after the grace period.
- **`usb.image_desktop_ps2`:** a desktop stick on a machine with only the
  i8042. The desktop starts, and Ctrl+Alt+Backspace on the PS/2 keyboard
  makes the compositor leave by itself.
- **`usb.image_desktop_no_keyboard`:** now removes the i8042 too (QEMU
  `i8042=off`).

## Audit (2026-11-04)

An independent read-only audit found no lock-order cycle (the interrupt
path takes PS2, RING, then STREAM; the lease takes LEASE, then STREAM;
STREAM is a leaf). It confirmed that no console byte can reach the stream.
It found one high, three medium and five low findings, all fixed in the
same branch:

- **H1:** both I/O APIC entries were routed only after the controller's
  interrupts were on. An edge on a still masked entry is lost on real I/O
  APICs, and the byte left in the shared output buffer would silence both
  ports for the whole boot. QEMU latches the edge, so no scenario could
  show it. Now the entries are routed first, and the buffer is drained
  through the handler once the interrupts are on.
- **M1:** the aux interface test required exactly 0. It now accepts `0xFA`
  and `0xFF` as Linux does, and the device's acknowledgement decides.
- **M2:** a key pressed during the aux bring-up could take the wheel ID's
  place, leaving 4-byte packets decoded as 3-byte ones. Each device is now
  set up with the other port disabled.
- **M3:** after an overflow the reset mark came before the entries queued
  before the loss, so a press could stay down. The mark now goes in-band,
  where the loss starts. The queue moved to `libs/input` (`Ps2Queue`) with
  host tests.
- **L1:** "only one process drains the stream" was not enforced; a
  `TRANSFER` right would let a second one read. The kernel now gives the
  stream to its first reader until that reader is gone. The docs state the
  real rights.
- **L2:** init could get the stream from a controller whose keyboard
  interrupt failed to route. What the kernel found is now published only
  after routing.
- **L3:** entries taken and not delivered were lost silently. They now
  leave a reset mark.
- **L4:** commands answered with "resend" are retried. The pointer
  resynchronizes after a 0.5 s pause. A self-reset pointer is the known
  limit above.
- **L5:** new tests: `Ps2Queue` on the host, and PS/2 typing at the shell
  after the desktop ends (`[host_input_then]` in
  `usb.image_desktop_ps2`). The kernel's 2 s net is covered by
  `gui.ps2_withheld` (`ps2=console`), and its code path is the same with
  the stream on. H1 can only be seen on hardware: a check on the laptop
  with a finger on the touchpad during boot.
