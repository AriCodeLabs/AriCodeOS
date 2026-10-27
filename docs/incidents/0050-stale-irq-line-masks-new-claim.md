# Incident 0050: an old claim's interrupt line masked the new claim's, and a moved MSI-X BAR was still programmed

- Opened: 2026-10-27. Status: fixed on `fix/pci-stale-irq-line`.
- Found by: the audit of incident 0047 (noted there under "What remains")
  and confirmed by its second, independent audit. Not seen on a machine.
- Environment: any boot. Defect 1 needs an interrupt line that outlives
  its claim; defect 2 a driver that moves a BAR of its function and dies.

One incident for two pre-existing PCI defects of the same pattern: what a
dead driver's claim left behind reached the next claim of its function.
Both were reproduced before the fix, on the code of `develop`
(`d9e982a`) with only the new test added.

## 1. Dropping an old line masked the new claim's interrupts

### Evidence

`test=pci_stale_line` (new scenario `pci.stale_line`): `pci_irq_keeper`
gets a claim's configuration and IRQ handles, sends the IRQ handle in a
message on an endpoint nobody receives from yet and faults. The reap
ends the claim (the configuration object was held only by the driver)
while the line stays in the queue; the kernel claims the function again,
then drops the endpoint with the message. On develop, with the virtio-blk
(MSI-X):

```text
pci: 00:02.0 1af4:1042 claimed again (the previous driver died) (MSI-X entry 0)
Reason: 00:02.0: dropping the old claim's line must leave the new claim's MSI-X entry on (vector 0x31)
```

And with the loop of the test restricted to the xHCI, which the scenario
starts with MSI only (`usb_hid_msi`), on develop:

```text
pci: 00:06.0 1b36:000d claimed again (the previous driver died) (MSI)
Reason: 00:06.0: dropping the old claim's line must leave the new claim's MSI on (vector 0x31)
```

The new claim's entry was delivered (address and vector 0x31, unmasked)
right after the claim and masked once the old line (vector 0x30) went.

### Mechanism

`IrqLine::drop` wrote entry 0 masked with address and data 0 (MSI-X), or
cleared the MSI enable bit, without checking that the line still owned
the source. The IRQ handle has `TRANSFER`, it is not revoked when the
function is claimed again, and `PciDevice::is_claimed` looks only at the
configuration object, so a line outlives its claim whenever it sits
elsewhere: in another process, in a queued message, or with a driver
that closed only its configuration handle. The next claim's
`register_msix` programs the same entry 0 with a new vector; the old
line's drop then silenced the new driver (its waits time out, a disk or
network driver hangs). The drop's write also took no lock while the new
claim's write held `VECTORS`, so the two writes could interleave into a
torn entry (for example the new address with data 0, unmasked). Normal
restarts never showed it: `reap` drops all the dead driver's handles
before init claims again.

### Fix

The line masks only what it programmed (ADR 0061, point 1). Its drop
reads the entry or capability and masks it only if it still holds the
line's own message: its vector towards the boot LAPIC. A vector is never
given to two live lines (`allocate_vector` skips the ones in use), so the
newer claim's entry always holds another vector. The read, the check and
the mask run under `VECTORS`, the lock both `register_msix` and
`register_msi` program under, so no entry can be torn. A drop that leaves
the source alone says so:

```text
irq: vector 0x30 released; its device was programmed again since (a newer claim), left as it is
```

Revoking the old line when the function is claimed again was considered
and not chosen (ADR 0061, "Alternatives"): it covers only the reclaim,
needs the device record to track the line and an ordering between the
PCI locks and `VECTORS`, whereas what the hardware holds decides exactly
whether the entry is still the line's, whatever way the line outlived
its claim.

## 2. The MSI-X table was programmed where its BAR had been

### Evidence

Same test, second part: `pci_bar_mover` turns memory decoding off through
its configuration page (so the moved BAR overlaps nothing) and writes a
new address, 128 MiB away, into the BAR that holds the virtio-net's MSI-X
table, then faults. The virtio-net advertises no FLR, so nothing restores
the BAR. On develop (with the first part left out of the loop, since it
fails first):

```text
pci: 00:03.0 BAR 1 moved since boot: not offered
pci: 00:03.0 1af4:1041 claimed again (the previous driver died) (MSI-X entry 0)
Reason: 00:03.0: a claim must not program the MSI-X table at 0x81043000, where its BAR was at boot (the entry there reads back as delivered: false)
```

The kernel wrote entry 0 at the boot-time address; the entry did not
read back there, since the function no longer decodes it.

### Mechanism

ADR 0051 made a claim check that the MSI-X capability register (BIR and
offset) reads as at boot, but `MsixTable::phys` is the BAR's boot-time
address plus the offset, and the BAR itself was not checked again, unlike
the BARs offered to the driver (`memory_bars`). Through its writable
configuration page a driver can move that BAR; on the next claim of a
function without FLR (an FLR restores the boot-time BARs) the kernel
programmed the entry at the old bus address, and the old line's drop
masked it there: uncached writes to an address nothing decodes, or that
another device's moved BAR decodes.

### Fix

A claim uses MSI-X only if the register and the BAR it names both read as
at boot (ADR 0061, point 2), with the same check that decides whether a
BAR is offered; otherwise the function gets MSI if it has it, else no
interrupt, logged:

```text
pci: 00:03.0 MSI-X table's BAR 1 moved since boot: MSI-X not used
```

A claim that does not use MSI-X now turns it off whether or not the
function has MSI (point 3; before, only when MSI followed): the old
claim left MSI-X enabled, and the device would go on sending the old
message, whose vector belongs to another line once the old one is gone.
The test checks MSI-X is off after the refused claim.

## Tests

- `pci.stale_line` (new, areas `pci`, `drivers`, `interrupts`): both
  parts above, plus that the newest claim's line, dropped, still masks its
  source (MSI-X and MSI). It failed on develop as shown and passes with
  the fix. Logs of the runs: `/home/serverbig/aricode-logs/incident-0050/`.
- `cargo xtask check`; `cargo xtask test --level unit` (718 host tests);
  `cargo xtask test --all --area pci,drivers,interrupts`: 28 scenarios, 0
  failed.
- Full suite on `20a326f` (kernel interrupt code): with 1 CPU, 200
  scenarios, 0 failed; with `--cpus 4`, 200 scenarios, 1 failed:
  `iommu.isolation` (below). Not rerun.

## Observed during the tests: `iommu.isolation` failed once with 4 CPUs

On `20a326f`, `cargo xtask test --all --cpus 4`, the iommu self-test
panicked because `dma_rogue` exited with 64:

```text
iommu: DMA fault from 00:02.0 (write at 0x371000): write to page without write permission (reason 0x5, unit 0)
dma_rogue: the device still reached the client buffer after its unmap
Reason: assertion `left == right` failed: dma_rogue: Exited(64)
```

The unit did block the write and the kernel recorded it (the first line),
yet `dma_rogue`, reading the fault count right after its request
completed, saw no new fault. The thermal guard paused nothing (75 °C at
most). Not seen before in the CI reports or the logs kept on the host,
and not in the 1-CPU run of the same commit.

Hypothesis (from the code, not yet confirmed by instrumentation): the
fault is counted by the unit's fault interrupt (`FAULT_VECTOR` 0x24), the
request's completion arrives as the virtio-blk's MSI-X (vector 0x30;
QEMU completes a request whose DMA faulted with an I/O error), and both go
to the boot CPU. With both pending, the CPU takes 0x30 first (higher
priority class); its handler wakes `dma_rogue`, which with 4 CPUs can run
on another CPU and read the count before the boot CPU takes 0x24. With 1
CPU `dma_rogue` cannot run until both interrupts are handled. VT-d
reports faults asynchronously, so the test (`dma_rogue`'s
`client_buffer`, and the kernel's `recorded >= 1` the same way) would
have to wait, bounded, for the count instead of reading it once.
Instrumentation that would confirm it: record the CPU and time of the
`FAULTS` increment and of `dma_rogue`'s read. Nothing in this branch
touches that path: the test's only interrupt line is claimed and
programmed as before, and dropped only after `dma_rogue` exits. It needs
its own incident and branch.

## What remains

- A line's drop after its own driver moved the BAR reads the entry at the
  old address: nothing decodes it there (so nothing is written), or a
  device whose driver arranged for it to echo the line's exact message,
  which then gets the mask itself (ADR 0061, "Alternatives").
- The holder of an old line can still wait on it. Before the function is
  claimed again it fires for the dead claim's device, as when it was
  handed over; after, never (the device sends another vector).
- The fix was not reviewed by an independent audit on this branch; the
  coordinator orders it before the merge (failure protocol, step 6).
- `iommu.isolation` with 4 CPUs (above): its own incident.
