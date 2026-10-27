# Incident 0051: dma_rogue read the DMA fault count before the fault was reported

- Opened: 2026-10-27. Status: fixed on `fix/iommu-fault-count-race`.
- Found by: the full suite with 4 CPUs on `fix/pci-stale-irq-line`
  (`20a326f`, incident 0050), one failure of `iommu.isolation`; then
  reproduced on develop `3d79e82` without that branch.
- Environment: QEMU with VT-d, `test=iommu`, more than one CPU.

## Evidence

`cargo xtask test --all --area iommu --cpus 4`, 25 runs on develop
`3d79e82` (logs `/home/serverbig/aricode-logs/iommu-repro/`): 24 passed,
run 25 failed. Its serial log:

```
[WARN ] iommu: DMA fault from 00:02.0 (write at 0x371000): write to page without write permission (reason 0x5, unit 0)
dma_rogue: the device still reached the client buffer after its unmap
[INFO ] user: process 1 'dma_rogue' exited with code 64
...
ARICODEOS KERNEL PANIC
Where: kernel/src/iommu.rs:763:5
Reason: assertion `left == right` failed: dma_rogue: Exited(64)
```

The panic is the kernel selftest's assertion on `dma_rogue`'s exit code
(how `test=iommu` reports a failure), not a kernel fault. The unit did
block the write and the kernel did count it: the line before
`dma_rogue`'s says so.

## Mechanism

`dma_rogue` (incident 0039's check of a client buffer given back) gives the
client buffer back, sends the device a request that writes into it, and
reads the kernel's fault count once the request completes. VT-d reports a
blocked access by writing a fault record and raising an interrupt; the
kernel counts it in that interrupt's handler (`iommu::on_fault_irq`, the
count raised before the warning is logged). The device's completion does
not wait for that: `dma_rogue` polls the used ring before it waits for the
completion interrupt, so on another CPU it can see the request done and
read the count before the boot CPU has handled the fault interrupt. With
one CPU the handler always runs first. The serial order fits: the count
was read, then the handler counted and logged, then `dma_rogue` printed
its failure.

A second weakness of the same test, found reading it: the check that DMA
to foreign memory was blocked compared the count with one taken at the
start of the program, before the client buffer step, whose own fault
could stand for it.

## Fix

The test, not the kernel: the fault report is asynchronous by design.
`dma_rogue` waits for the count to rise above the value taken before the
request, up to 1 s (`faults_above`), wherever a fault is expected; where
none is expected it reads the count after 100 ms (`faults_settled`), so a
late fault is counted rather than missed. The foreign-memory check counts
from just before its own request.

## Verification

`cargo xtask test --all --area iommu --cpus 4`, 50 runs, and 5 runs with
one CPU, on the fix (logs `/home/serverbig/aricode-logs/iommu-fix/`): 0
of 50 failed with 4 CPUs (1 of 25 before the fix), 5 of 5 passed with one
CPU. The change is to a test program only; no kernel code moved.
