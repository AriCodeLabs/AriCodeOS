# Incident 0039: a driver's device could still reach a client buffer the driver had unmapped

- Opened: 2026-10-25. Status: fixed on `fix/init-supervisor`.
- Found by: the failure-pattern audit of 2026-10-25 (audit C, L11: an
  authority granted for a use and never taken back when the use ends).
  Not seen on a machine.
- Environment: machines with an IOMMU (VT-d); QEMU with `intel-iommu`.

## Evidence

`iommu.isolation` extended with a client buffer: the kernel hands
`dma_rogue` a DMA buffer it did not allocate; the device reads sector 0
into it while it is mapped, then `dma_rogue` unmaps it and asks for the
same read again. On the kernel at `df15084`:

```text
dma_rogue: legitimate read ok, DMA to foreign memory blocked (1 fault(s) recorded)
dma_rogue: the device still reached the client buffer after its unmap
```

## Mechanism

`mmio_map` of a foreign DMA buffer adds it to the process's IOMMU domain
(ADR 0038), so the driver's devices can use it. `mmio_unmap` removed the
CPU mapping and the reference only: the domain kept the buffer until the
buffer was freed or the driver exited. While the client still held the
buffer, a driver that had given it back (`ClientBuffers` evicting it, a
request finished) still had a device that could read or write it, for
instance after the client reused the buffer for its next request to
another service.

## Fix

`map_region` reports whether the call put the region in the domain; the
mapping records it (`Foreign::in_domain`, matched by physical address so a
concurrent unmap and map in another thread cannot mark the wrong record).
When the mapping goes, the region leaves that process's domain
(`iommu::unmap_region`, IOTLB invalidated), unless the process still holds
a handle to the buffer: a driver keeps the handle of a buffer its device
may still own (incident 0017, `DeviceOwned`, "kept for good"), so the
region then stays until the last handle is closed or given away
(`settle_lingering`, after `handle_close` and `ipc_send`), or until the
buffer is freed. A buffer the process allocated itself stays in its
domain, as before.

## Audit

The independent audit of the first version found that removing the
region with the mapping broke incident 0017's invariant: `ClientBuffers`
evicting the mapping of a buffer an xHCI or EHCI controller still owned
would have made the stalled transfer fault, and the controller halt,
instead of landing in the kept buffer. Fixed as above; `iommu.isolation`
now checks both sides: the device reaches the buffer while its handle is
held after the unmap, and faults once the handle is closed. The same audit
moved the client-buffer check before the illegal DMA, so a request the
device never completes cannot leave the queue stuck for it.
