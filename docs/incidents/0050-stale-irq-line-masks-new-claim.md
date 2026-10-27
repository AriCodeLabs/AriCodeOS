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

And with the loop of the test restricted to the xHCI, which the first
version of the scenario started with MSI only (`usb_hid_msi`; it now
offers both and the test parks its BAR so that it gets MSI), on develop:

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

A line touches its function's interrupts only while its claim is the
latest (ADR 0061, point 1). Under `VECTORS` the kernel keeps a number
per PCI function that every claim changes (`register_msix`,
`register_msi`, or `claimed_without_interrupt` for a claim with no
line), and each MSI-X or MSI line records its claim's number. The drop
masks only if that number is still the function's, and decides it
without reading the device. The decision and the mask run under
`VECTORS`, the lock every claim programs under, so no entry can be torn.
A drop that leaves the source alone says so, after releasing the lock:

```text
irq: vector 0x30 released; its function was claimed again since, its interrupts left to the newer claim
```

The first version of the fix read the entry in the drop and masked only
if it still held the line's vector; the audit of the branch showed what
those reads cost (see "Audit"), and the claim numbers replaced them.
Revoking the old line on reclaim was not chosen (ADR 0061,
"Alternatives").

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

A claim uses MSI-X only where the function decodes the table (ADR 0061,
point 2): the register and the BAR it names both read as at boot (the
same check that decides whether a BAR is offered) and memory decoding is
on after the claim's command write (added after the audit, below);
otherwise the function gets MSI if it has it, else no interrupt, logged:

```text
pci: 00:03.0 MSI-X table's BAR 1 moved since boot: MSI-X not used
pci: 00:04.0 memory decoding stays off (a BAR was not offered): MSI-X not used
```

A claim that does not use MSI-X now turns it off whether or not the
function has MSI (point 3; before, only when MSI followed): the old
claim left MSI-X enabled, and the device would go on sending the old
message, whose vector belongs to another line once the old one is gone.
The test checks MSI-X is off after the refused claim.

## Audit (2026-10-27)

An independent audit of `20a326f` and `b1f88f8`, ordered by the
coordinator, confirmed both fixes, that the tests fail without them, and
that nothing in the branch explains the 4-CPU `iommu.isolation` failure
(below). Its findings, each fixed on this branch after merging develop
(`a789243`), the first two reproduced first:

- **Medium (pre-existing): MSI-X was programmed while memory decoding
  could be off.** A claim turns decoding on only when every assigned BAR
  is offered. A driver that moved another BAR (the virtio keyboard's BAR
  4) and turned decoding off left the table's BAR in place: the next
  claim took MSI-X, its writes went nowhere and MSI-X was enabled with
  whatever entry 0 held (the dead claim's vector, maybe another line's by
  then), firing once a driver turned decoding and bus mastering on. With
  `pci_bar_mover` moving the keyboard's BAR 4, on `a789243`:

  ```text
  pci: 00:04.0 BAR 4 moved since boot: not offered
  pci: 00:04.0 1af4:1052 claimed again (the previous driver died) (MSI-X entry 0)
  Reason: 00:04.0: a claim must not program the MSI-X table at 0x81042000 after BAR 4 moved with decoding off (the entry there reads back as delivered: false)
  ```

  Fixed: MSI-X also needs memory decoding on after the claim's command
  write (ADR 0061, point 2); otherwise MSI or nothing, and MSI-X off.
  The dead line's drop had also read all ones there and logged "a newer
  claim", which was untrue; the drop no longer reads (next point but one).
- **Low (reachable through the branch): MSI and MSI-X both enabled.** An
  MSI claim's line that lives on no longer turns MSI off once the
  function is claimed again, and the MSI-X path never cleared MSI
  Enable. The test parks the xHCI's BAR so that a claim gets MSI, keeps
  that line, puts the BAR back and claims again; on `a789243`:

  ```text
  pci: 00:06.0 1b36:000d claimed again (the previous driver died) (MSI-X entry 0)
  Reason: 00:06.0: a claim that takes MSI-X must turn off the MSI an earlier claim left enabled (its line still lives)
  ```

  Fixed: the MSI-X path turns MSI off before enabling MSI-X (ADR 0061,
  point 3).
- **Low: the drop read the device with `VECTORS` held and interrupts
  off.** Non-posted reads could stall on a function within 100 ms of an
  FLR (until the completion timeout, while other CPUs spin on `VECTORS`
  in `handle_device_irq`) or reach another device that moved a BAR onto
  the old table address (read side effects). Fixed by removing the reads:
  ownership comes from the claim numbers (Fix of defect 1). Only the
  test's `hw::delivered` reads the device, outside any drop.
- **Low: `info!` inside the `VECTORS` critical section.** The drop now
  logs after releasing the lock.
- **Info: the `msix_window` comment** said the dead line masks its entry
  unconditionally; corrected.

With the claim-number check disabled, `pci.stale_line` fails at the
original assertion (`dropping the old claim's line must leave the new
claim's MSI-X entry on (vector 0x31)`), so the scenario still tells the
fix apart.

## Tests

- `pci.stale_line` (new, areas `pci`, `drivers`, `interrupts`, xHCI with
  MSI and MSI-X): the stale lines (MSI-X on the virtio-blk, MSI on the
  xHCI with its BAR parked), the newest line still masking its source,
  MSI off under MSI-X, and a moved BAR (the table's on the virtio-net,
  another on the virtio keyboard) with decoding off. Each part failed
  before its fix as shown and passes with it. Logs of the runs:
  `/home/serverbig/aricode-logs/incident-0050/`.
- On `20a326f`: `cargo xtask check`; `cargo xtask test --level unit` (718
  host tests); `cargo xtask test --all --area pci,drivers,interrupts`: 28
  scenarios, 0 failed; full suite with 1 CPU, 200 scenarios, 0 failed;
  with `--cpus 4`, 200 scenarios, 1 failed: `iommu.isolation` (below).
  Not rerun.
- On `f580dd4` (the audit follow-up, after merging develop): `cargo xtask
  check`; `cargo xtask test --level unit` (726 host tests); `cargo xtask
  test --all --area pci,drivers,interrupts`: 28 scenarios, 0 failed; full
  suite with 1 CPU, 202 scenarios, 0 failed; with `--cpus 4`, 202
  scenarios, 0 failed (`iommu.isolation` passed this time; its failure
  above stays open). Host at most 80 °C, the thermal guard paused
  nothing.

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
programmed as before, and dropped only after `dma_rogue` exits (the
audit agreed). The coordinator is reproducing it on develop as its own
incident.

## What remains

- A drop of the latest claim's line still writes (posted) its MSI-X entry
  at the boot-time table address. If its own driver moved that BAR, the
  write goes where the BAR was: nowhere, or into a function whose driver
  moved a BAR there, which the kernel cannot see without reading (ADR
  0061, "Consequences"). The same write was made before this incident.
- The holder of an old line can still wait on it. Before the function is
  claimed again it fires for the dead claim's device, as when it was
  handed over; after, never (the device sends another vector or nothing).
- The kernel does not read entry 0 back after programming it (ADR 0061,
  "Alternatives").
- `iommu.isolation` with 4 CPUs (above): its own incident.
