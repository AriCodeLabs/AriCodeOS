# Incident 0033: drivers trusted device-written indexes and offsets, and leaked request handles

- Opened: 2026-10-25. Status: fixed on `fix/driver-bounds`.
- Found by: the failure-pattern audit of 2026-10-25 (audit A: M1, L1,
  L2; audit B: L6; audit C: M3, L7). Not seen on a machine.
- Environment: the virtio drivers (block, network, input), the xHCI and
  EHCI drivers.

## Evidence

- virtio (A-M1, reproduced by the audit on the host): `Virtqueue::pop_used`
  used the `id` the device writes in the used ring as a descriptor index
  with no bound; with id 1000 in a 64-entry queue it wrote a `u16` 0x3e8e
  bytes past the queue and the next `add` a whole descriptor there. It
  then walked the chain through the descriptor table, which the device
  can also write: a cycle hung the driver, a repeated id corrupted the
  free list.
- virtio-pci (A-L1): structure offsets and `queue_notify_off ×
  notify_off_multiplier` (up to 2^48) were never compared with the BAR;
  a capability naming BAR 6 or more shifted past the driver's BAR mask.
- xHCI/EHCI (A-L1, A-L2): RTSOFF, DBOFF and CAPLENGTH placed register
  blocks with no bound; the slot id from Enable Slot indexed the DCBAA,
  which holds `max_slots + 1` entries, unchecked.
- USB drivers (C-M3): extra handles in requests were never closed: about
  85 requests carrying three each filled the driver's table, after which
  requests arrived without their reply endpoint and the keyboard and USB
  storage stopped until reboot.
- virtio-blk (C-L7): a timed-out request was answered before the device
  was reset, so a late DMA could land in a buffer the client had reused.

## Fix

- `Virtqueue` keeps its own copy of the free list's links and, per chain
  the device holds, its length and writable bytes; `pop_used` returns
  `UsedError` for an id that is no held head or a length past the
  chain's capacity. virtio-blk fails the request and resets the device;
  virtio-net and virtio-input exit so init relaunches them (which resets
  the device). Queues are at most `MAX_QUEUE_SIZE` (64) entries.
- virtio-pci structures must lie within their BAR (`SetupError::
  BadStructure`), and so must each queue's notification register.
- `aricode_xhci::Params::registers_fit` and `aricode_ehci::Params::
  registers_fit` bound the register blocks by BAR 0's length; Enable Slot
  must name a slot within `max_slots`.
- usb-input and usb-ehci close every handle of a request they do not use.
- virtio-blk resets the device before answering a request it may still
  complete.
- Host tests: `device_made_up_used_elements_are_refused` (virtio),
  `register_blocks_must_fit_the_bar` (xHCI),
  `operational_registers_must_fit_the_bar` (EHCI).
