# ADR 0023: USB storage as a block service inside usbd

- **Date:** 2026-10-10
- **Status:** Accepted

## Context

On the test laptop (Phase 34) the only disk AriCodeOS can use is its own
USB stick, and the only block driver was virtio-blk. Without a block
service there is no fsd, the slot is not confirmed and init does not
launch the shell: the sixth physical boot ended at
`ARICODEOS_KERNEL_READY` with nothing to do. In addition, on Intel 7
series the USB ports start out routed to the EHCI controllers (which have
no driver) and the xHCI saw no device at all.

## Decision

1. **The xHCI driver (`usb-input`, "usbd") also handles mass storage.** A
   controller has a single owner, so the storage class lives in the same
   process as HID: it enumerates the first bulk-only interface (class 8,
   subclass 6, protocol 0x50), configures its bulk endpoints, identifies
   the device (`INQUIRY`), waits until it is ready (`TEST UNIT READY` with
   `REQUEST SENSE`) and reads the capacity (`READ CAPACITY(10)`; 512-byte
   blocks only). At most one device.
2. **Transport and SCSI in a host library** (`aricode-usb-storage`:
   CBW/CSW and the `INQUIRY`, `TEST UNIT READY`, `REQUEST SENSE`, `READ
   CAPACITY(10)`, `READ(10)`, `WRITE(10)`, `SYNCHRONIZE CACHE(10)` CDBs),
   with tests. `aricode-xhci` adds bulk endpoints, TDs that do not cross
   64 KiB (`bulk_trbs`) and the recovery commands (Reset Endpoint, Set TR
   Dequeue, `CLEAR_FEATURE(HALT)`) that BOT requires after a STALL.
3. **Second extra in the driver contract: the block endpoint.** init
   passes each xHCI, after `SYSTEM`, an endpoint that usbd serves with
   `aricode-proto::block` (`Info`, `Read`, `Write`, `Flush` =
   `SYNCHRONIZE CACHE`) alongside input, with `ipc_receive_any`. With no
   device, `Info` answers `Invalid`. The extra is optional: the driver
   checks that the handle is an endpoint (the kernel test bench does not
   pass it).
4. **Without virtio-blk, init uses the USB block device.** After launching
   the xHCIs, if there is no block service it asks the first one for its
   capacity; if it answers, it becomes the block service and the same
   stack runs on top of it (fsd on ARI0, test clients, slot confirmation)
   as with virtio-blk (`start_storage_stack`).
5. **Firmware handoff and Intel routing.** Before touching the controller,
   usbd requests ownership through the USB Legacy Support capability and
   disables its SMIs; on Intel xHCIs it writes `XUSB2PR` and `USB3_PSSEN`
   with the `USB2PRM`/`USB3PRM` masks (as Linux does) so the shared ports
   move from the EHCIs to the xHCI, and it waits longer before
   enumerating.
6. **Scenario `usb.storage`** (`usb_storage = true`): the test disk hangs
   off QEMU's xHCI as `usb-storage` and there is no virtio-blk; fsd and the
   shell run over usbd and a written file survives a reboot.

## Consequences

- A USB stick attached to the xHCI is the system disk on the laptop: the
  same stick AriBoot boots from. Writes go only there (section 95).
- Block requests are serialized with HID input in the same loop; a USB
  keyboard stays responsive because each block request takes
  milliseconds.
- `READ(10)`/`WRITE(10)` limit the disk to 2 TiB; native 4 KiB sectors are
  not supported.
- Seventh photo from the laptop: the stick is read but `fs_client` fails
  when creating a file. Many USB sticks reject `SYNCHRONIZE CACHE` as an
  illegal request (they have no cache to flush): usbd detects this from
  the sense data (`Sense::command_unsupported`), reports it once and flush
  becomes immediate; other rejected commands are reported with their sense
  data so they can be diagnosed from a photo.
- If usbd dies, the supervisor relaunches it and fsd sees I/O errors until
  then; the block endpoint belongs to init and survives.
