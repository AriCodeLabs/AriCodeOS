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
mapping records it (`Foreign::in_domain`), and `mmio_unmap` takes the
region out of that process's domain (`iommu::unmap_region`, IOTLB
invalidated) right after the CPU mapping goes. A buffer the process
allocated itself stays in its domain, as before. Drivers keep the handle,
not the mapping, of a buffer their device may still own (incident 0017),
and serve one request at a time, so no in-flight transfer loses its
buffer.
