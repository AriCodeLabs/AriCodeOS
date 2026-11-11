# xHCI hot-plug: device study (2026-11-11)

Why: on the owner's office laptop (Lenovo V15 G2 ITL, Tiger Lake PCH,
xHCI `8086:a0ed`) a USB mouse plugged in after boot was not seen ("no
reconoce el ratón, tengo que reiniciar"). usbd enumerated the root ports
once at boot; a Port Status Change only printed PORTSC and re-enumerated the
storage device's own port. A HID unplugged kept its slot and its keys.
Decision record: ADR 0087. Skill followed: `aricodeos-device-study`.

## 1. The device

- Any xHCI usbd drives (interface `0x30`): QEMU's `qemu-xhci` in the tests,
  the 2012 reference laptop (`8086:1e31`), the office laptop's Tiger Lake
  PCH xHCI (`8086:a0ed`). Hot-plug uses only what every xHCI has: root
  port registers (PORTSC) and Port Status Change Events. No Intel-specific
  register is written for it.
- Tiger Lake also has a Type-C subsystem xHCI (`8086:9a13`); usbd runs one
  instance per xHCI, so both get the same logic.
- Not probed on real hardware in this branch (the office laptop is not
  reachable from the build host). The driver's console lines say what it
  did with each change (section 8), so a report reads remotely.

## 2. Official documents

- **xHCI 1.2** (Intel, 2019). Downloading it on 2026-11-11 was refused by
  Intel's CDN bot check from this host, and the check was not worked
  around. Section numbers below come from the citations the driver
  already carries (incident 0001 study, ADR 0023) and from the veterans'
  comments, which quote the same revision; they should be read against
  the PDF when the owner has it at hand:
  - 4.3 Device slot initialization (port reset, Enable Slot, Address
    Device, Configure Endpoint); 4.3.3 zeroed Output Device Context.
  - 4.6.3 Enable Slot, 4.6.4 Disable Slot (the slot's contexts and rings
    belong to software again only after a successful completion), 4.6.5
    Address Device, 4.6.9 Stop Endpoint (the TD in progress is retired
    with a Stopped Transfer Event), 4.6.11 Reset Device.
  - 4.19.1 root hub port state machines (USB2: Disconnected, Disabled,
    Reset, Enabled; USB3: the link trains to U0 by itself, Polling,
    SS.Inactive and Compliance Mode need a Warm Reset); 4.19.2 Port Status
    Change Event generation: an event when a change bit goes 0 to 1, so
    every change bit must be cleared or no further event comes.
  - 5.4.8 PORTSC: CCS, PED (RW1C: writing 1 disables the port), PR, PLS,
    PP, speed, CSC, PEC, WRC, OCC, PRC, PLC, CEC, CAS (USB3, RO), WPR
    (USB3, RW1S; reserved on USB2 ports).
- **USB 2.0**: 7.1.7.3 connect debounce: at least 100 ms between the
  connection and the reset, restarted on every disconnect seen in that
  interval (TATTDB); 7.1.7.5 reset of at least 10 ms; 11.24.2.7.2 port
  change bits (C_PORT_CONNECTION, C_PORT_ENABLE set when a port error
  disables the port).
- **USB 3.2** link states as PORTSC reports them (PLS values 0 U0, 5
  RxDetect, 6 SS.Inactive, 7 Polling, 10 Compliance Mode).

## 3. Veterans and their failures (source read only, nothing built)

Sources fetched into `~/aricode-scratch/xhci-hotplug-study/src/` (not in
the repository). Linux is GPL: studied, not copied; FreeBSD (BSD-2) and
Haiku (MIT) could be adopted, but nothing needed copying.

| Who | What they do | What went wrong for them / what we take |
|---|---|---|
| Linux `hub.c` `hub_port_debounce` | Polls every 25 ms until the connection is unchanged for 100 ms, gives up after 2000 ms ("connect-debounce failed") | The idea, event driven: every connect change restarts the 100 ms (`PendingPorts::changed`); no polling thread, no timeout needed because a bouncing port only keeps its own entry busy |
| Linux `port_event` | PEC with the port disabled under a device: "disabled by hub (EMI?), re-enabling" and a re-enumeration | Taken (`port_action`: `detach` + `debounce`) |
| Linux `hub_port_warm_reset_required`, `xhci-hub.c` CAS handling, FreeBSD `UPS_PORT_LS_SS_INA` | Warm reset for a USB3 port in SS.Inactive or Compliance Mode, or with Cold Attach Status; Linux also fakes Compliance on CAS and has `XHCI_COMP_MODE_QUIRK` for parts that enter Compliance without CAS | Taken (`port_reset`); the link state is ignored while PR is set (Linux `handle_port_status`: not reliable during a reset). The compliance-mode timer quirk (TI/some Intel parts after S3) is not needed without suspend |
| Linux `hub_port_connect_change` | Tries to "resuscitate" a device on CSC if its descriptors read the same | Not taken: for boot HID a fresh enumeration loses nothing (held keys are released), and treating CSC as a new device cannot keep a stale slot for a different device |
| Linux `hub_port_connect` | `PORT_INIT_TRIES` 4 attempts, power cycle halfway, "unable to enumerate USB device" | Retries with a port reset, `hotplug::Policy::attempts` 3 (Haiku makes 2); no port power cycle (PPC is optional and not read) |
| Linux `hub_port_reset` | Waits TRSTRCY 10 ms "plus some extra" (50 ms in all) after a reset before addressing the device | Taken: `Policy::reset_recovery_ms` 50 (10 to 200), on top of the reset's own 500 ms window (`RESET_WINDOW_MS`), never cut short |
| Linux `xhci_queue_ctrl_tx` | Sets ISP on an IN data stage, so a short descriptor reports how much arrived | Taken (`Trb::data_stage`); the driver reads only the bytes received, never what an earlier device left in the shared buffer |
| Linux `xhci_free_dev` / `xhci_disable_slot` | Frees the virt device even when Disable Slot fails ("Unsuccessful disable slot") | Guarded already (incident 0017, audit 5 M1): memory becomes a spare only after Success or Slot Not Enabled, otherwise it is abandoned with a line |
| Linux input core (`input_dev_release_keys`) | Releases the keys a vanishing device held | Taken: `release_all` at the moment the port change is seen, before the slot goes |
| FreeBSD `uhub_reattach_port` | Clears C_CONNECTION, frees the old child first, waits the power-up delay, resets; if the connection changed during the reset it tries once more ("device vanished") | The order (old device out first) is ours too: a gone device is detached before its port is enumerated again |
| Linux `hub_port_wait_reset` / `hub_port_reset` | A reset is over when PR is clear and the port connected; a USB2 C_CONNECTION while still connected resets again (-EAGAIN, up to `PORT_RESET_TRIES` 5); after a SuperSpeed reset C_CONNECTION is ignored ("a USB 3.0 connection may bounce if multiple warm resets were issued") | Taken (`hotplug::after_reset`, `reset_port`): the change bits are cleared before each reset, a USB2 change during the reset or its recovery resets again up to `Policy::reset_tries`, a USB3 one is cleared with the others; the port's protocol comes from the Supported Protocol capabilities (xHCI 7.2, `PortProtocols`), as Linux keeps a root hub per protocol. When the tries run out the change bits are cleared, so the bounces neither re-queue the port with fresh attempts nor restart the storage's count. A first version failed the enumeration on such a change and could lose the boot stick; a second one left the change bits set after the last try (unbounded retries); the audits caught both |
| FreeBSD `xhci_device_uninit` | Disable Slot, free the device extension | Same order, with the completion checked |
| Haiku `Hub::Explore` / `_DebouncePort` | Polls the ports; debounces with a stable time; 2 tries; disables the port on failure | We act on events instead of polling (Haiku's xHCI ignores Port Status Change Events and only traces them, so a change waits for the next explore round) |
| Haiku `XHCI::CleanupDevice` / `_RemoveEndpointForPipe` | Stops endpoints, Disable Slot, deletes the memory without checking the completion | Endpoints stopped first, as here; completion checked (above) |
| Every one of them | Transfer errors when the device disappears are reported as such | `ended_by_unplug`: a failed HID transfer on a device that is gone, or whose port has no connection, is the unplug, not a fault to record or recover (the EHCI driver's `unplugged` does the same) |

Other failure modes guarded:

- An event whose port number is out of range is ignored (already there).
- Change bits are cleared on every event, so the next change raises an
  event (xHCI 4.19.2); a change seen in PORTSC when a debounce ends (an
  event not handled yet) restarts the debounce instead of enumerating.
- The boot enumeration takes the connection of each port it enumerates:
  a change event queued for that port while an earlier one was being
  enumerated is dropped then (`PendingPorts::forget`).
- Slot leaks: a failed enumeration still disables its slot (already
  there); the keyboard and mouse counts it raised are lowered with it
  (`forget_inputs`, which also fixes the boot path).
- A Disable Slot never overlaps another one (a second would replace the
  first one's bookkeeping and abandon its memory): a detach, an
  enumeration and a storage re-enumeration from the idle loop wait while
  one is outstanding (`disable_outstanding`); one asked from elsewhere (a
  block request giving up the storage's endpoint, a late Enable Slot)
  waits in line (`waiting_disables`). One without a completion in 5 s
  (Linux `XHCI_CMD_DEFAULT_TIMEOUT`) is given up, its memory and buffers
  kept for good, so hot-plug cannot stall behind a lost completion.
- A device unplugged or replaced while it was being enumerated: every
  entry on a port that sees a connect change is flagged, and the HID
  enumeration checks `hotplug::still_attached` before it queues reports,
  failing (its slot released) when the device there is not the one it
  addressed; the port is debounced again (also after a port disabled under
  it, which raises no connect change), at most `Policy::attempts` times in
  a row (`PortBounces`), then left alone until it is seen empty. A HID
  whose port has no connection although no change was seen is taken as
  gone by its first failed report, which releases its keys at once.
- A data stage reported as Success with a residual (Etron, ASMedia, VIA,
  Renesas, as Linux notes) counts as short, unless a Short Packet came
  first (Panther Point's spurious Success after one changes nothing).
- A change whose event was lost leaves CSC set: the enumeration due on that
  port handles it as the event would (`port_change`: a served device is
  gone, the storage's reconnect logic runs) and debounces again.
- An Enable Slot that completes after its enumeration gave up hands out a
  slot nobody uses: it is disabled when the late completion arrives.

## 4. Real hardware

No probe ran (no access). Nothing in this work writes a register the
driver did not write before, except PORTSC.WPR, which is written only for
a USB3 port whose PLS reads SS.Inactive or Compliance or whose CAS is set.

## 5. Resources

| Resource | Use |
|---|---|
| Interrupts | Port Status Change Events wake the driver (MSI/MSI-X, ADR 0044); no port polling |
| Memory | Slot memory now carries the HID rings and report buffers and is pooled (`SPARE_SLOTS` = `MAX_DEVICES` + 1) once released: plugging and unplugging does not grow the arena (QEMU: 2880 bytes for the first mouse, 0 for every later device on a released slot) |
| Time | Each attach or detach is one idle-loop step: attach bounded by `ATTACH_STEP_MS` (3 s; a HID takes a few ms in QEMU), detach by `MAINTENANCE_STEP_MS`. The timings and counts are one `hotplug::Policy` (debounce 100 ms, reset recovery 50 ms, 3 attempts, retry pause 200 ms times the failed attempts) |
| Ports | One pending entry per port waiting (`PENDING_PORTS` 16); a full queue is refused on the console |
| Power | Port power switching and link power management are not used (PPC, U1/U2 left as reset) |

## 6. Fail safe

- A device beyond `MAX_DEVICES` (4) is refused with
  `usb-input: port N: device not enumerated: all 4 device entries are in use (MAX_DEVICES)`.
- Final refusals are not retried: no boot HID or storage interface,
  storage already present, storage plugged in after boot, too many devices.
- Every failure prints `usb-input: port N: enumeration failed: WHY`, then
  either the retry or that the port waits for a new connection; real
  failures are also recorded (`usb attach fail`).

## 7. Left out (owner decisions)

- **Storage plugged in after boot** is not served: init chooses its disks
  at boot and never asks again, and serving a disk nobody opens would only
  hold the idle loop for its INQUIRY / TEST UNIT READY. The storage present
  at boot keeps its own logic: it is re-enumerated on its own port only;
  plugged back into another port it is refused with a line naming the
  port it was on. Serving later disks needs init to watch block services.
- **External hubs on the xHCI** are not enumerated (a hub is "no boot HID
  or storage interfaces"). Hub support needs: the hub class requests
  (`GET_DESCRIPTOR(HUB)`, `SET_FEATURE(PORT_POWER)`, `GET_STATUS` and
  `CLEAR_FEATURE(C_PORT_*)` per port, as `libs/ehci::hub` already has for
  the EHCI driver and could be shared); the hub's status-change interrupt
  endpoint; Slot Context fields for devices behind it (Route String,
  parent hub slot and port for full/low speed behind a high-speed hub's
  transaction translator, MTT, TT think time) and the hub bit and number
  of ports set with Evaluate Context (xHCI 4.5.2, 6.2.2); a detach that
  takes the devices below a hub with it; and a larger `MAX_DEVICES`.
- **Desktop session**: init decides at boot between the console and the
  desktop by whether a keyboard is present; a keyboard plugged in later
  types into whichever was chosen, but does not start the desktop.
- Port power cycling on repeated failures (Linux does it halfway through
  its retries) and over-current handling (the change is printed only).
- The touchpad of the office laptop is I2C-HID, not USB: separate work.

## 8. Tests and expected lines

- Host: `aricode-xhci` `hotplug::tests` (port actions, reset kind,
  debounce queue with retries and deferrals that never overwrite a newer
  connection, the policy, `still_attached`, spare pool), the data stage's
  ISP, `aricode-input` `release_all` tests, xtask hot-plug stage parsing
  and input name checks.
- QEMU `usb.xhci_hotplug` (level release): mouse plugged in, moved,
  unplugged; a mouse on another port reusing slot and memory, unplugged with
  its button held (released); a keyboard on the first port; three more
  mice; a fifth device refused; the hot-plugged keyboard types `shutdown`.
  Each stage waits for the line the previous one causes.
- QEMU `usb.xhci_hotplug_storage` (level release): the same xHCI serves the
  boot stick while 600 writes run; a mouse, a keyboard and a second mouse
  fill the four entries with the stick; a third mouse is refused; a second
  stick is refused (`a storage device is already present`); writes go on
  after it, and the hot-plugged keyboard types `cat probe.txt` and
  `shutdown`.
- A stage that never ran or that QEMU refused fails every scenario
  (`test.rs`), not only those that list its line.
- Expected lines on a real machine:

```text
usb-input: port 5 status change: PORTSC ... CCS=1 ... CSC=1 ...
usb-input: port 5: device plugged in; enumerating (attempt 1)
usb-input: port 5: HID mouse (boot protocol, 4 bytes per report, endpoint 1)
usb-input: port 5: device attached on slot 1, 0 new bytes of the DMA arena
usb-input: port 5: the device on slot 1 is gone; the idle loop detaches it
usb-input: port 5: device on slot 1 detached
```
