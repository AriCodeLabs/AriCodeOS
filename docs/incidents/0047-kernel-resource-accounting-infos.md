# Incident 0047: stale handles, DMA charges, MSI-X windows and console input outlived what they belonged to

- Opened: 2026-10-27. Status: fixed on `fix/kernel-infos`.
- Found by: the failure-pattern audit of 2026-10-25 (section "Info", the
  first four: state that survives the object, request or session it
  belonged to). Not seen on a machine.
- Environment: any boot. Each one needs a long run, a failing request or a
  crash to show.

One incident for four small findings of the same pattern; a section each.
Every one was reproduced before the fix, on the code of `develop`
(`5ef2f6e`) with only the new test added.

## 1. A closed handle came back after 65535 reuses of its slot

### Evidence

Host test `a_stale_handle_never_resolves_again` (`libs/handles`): a
handle is inserted and removed, then one handle is inserted and removed
again and again, as a program that opens and closes one object in a loop
does. On develop it failed at the 65535th reuse:

```text
assertion `left != right` failed: the closed value was handed out again after 65535 reuses
  left: Handle(65537)
 right: Handle(65537)
```

The stale value `0x10001` named the new object again: `get(stale)`
resolved to it.

### Mechanism

`HandleTable` kept its free indexes in a `Vec` used as a stack: the index
just freed was the next one handed out (LIFO), and its 16-bit generation
wrapped (`wrapping_add(1).max(1)`) after 65535 closes. A request/reply
client creates a reply endpoint per call (`aricode_rt::rpc`), so a busy
client reuses one index on every call and reaches the wrap after 65535
calls: about five seconds at the 70-80 us per call `cargo xtask perf`
measures under QEMU. Any stale value it kept (a handle closed by one
part of the program, still stored in another) then named whatever object
the slot held.

### Fix

The handle format stays (`generation << 16 | index + 1`), but values are
spread over all of it (ADR 0057):

- the table uses each of its positions (up to its limit, 256 in the
  kernel) once before reusing any, and then the one free the longest
  first (a `VecDeque`, FIFO);
- position `p` hands out the indexes `p + limit * epoch`: when its
  generation runs out the position moves to its next index (epoch) with
  generation 1, and only once every index of the position has had every
  generation does it start again.

A value then comes back only after its position went through all its
other values: 255 or 256 * 65535 reuses of that position with 256
positions. Spread by the FIFO over the free positions, that is between
about 1.7 * 10^7 closes in the table (every other position held) and
about 4 * 10^9 (every other position free), instead of 65535. The low half
of a handle never reads `0xFFFF`, so `endpoint_create_pair`'s packed
result still never reads as an error (its comment and ADR 0030 relied on
indexes staying below `HANDLE_LIMIT`). The values of a fresh table are
unchanged (`0x10001`, `0x10002`, ...), so the ABI's fixed handles and the
ones a spawn passes keep theirs. Host tests: the stale value never comes
back with 1, 4 or 256 positions over 3 * 65536 reuses; positions take
turns oldest first; one position goes through all its 4 * 65535 values
once before repeating.

## 2. A failed DMA allocation kept its quota charge

### Evidence

`test=dma_lifetime` step 4 (scenario `memory.dma_lifetime`): `dma_refund`
runs with a quota of two pages, fills its handle table, asks four times
for one page (each request is charged, then fails because no handle fits)
and asks once more after freeing one entry. On develop:

```text
dma_refund: the request within the quota error 7
user: process 4 'dma_refund' exited with code 4
assertion `left == right` failed: dma lifetime: failed allocations must give their quota charge back (exit status, DMA charged)
  left: (Exited(4), Some(8192))
 right: (Exited(0), Some(4096))
```

### Mechanism

The `dma_alloc` syscall charged the quota first (`charge_dma`) and then
could fail three ways: no contiguous memory, the IOMMU mapping, and the
handle insert. None gave the charge back, and the quota is cumulative, so
a driver whose allocations failed under memory pressure or with a full
table lost its quota for good.

### Fix

Everything after the charge runs in one closure; any error gives the
charge back (`process::refund_dma`). A buffer the process got still stays
charged after it is freed (ADR 0038 unchanged on that).

## 3. Every claim of a PCI function mapped a new MSI-X window

### Evidence

`test=pci_reap` (scenario `drivers.pci_reap`) now claims each function
nine more times after its first claim, as a restarted driver does, and
compares `hw::uc_window_bytes()` before and after. On develop:

```text
assertion `left == right` failed: claims after the first one map no new uncached window
  left: 45056
 right: 8192
```

One page more per claim, never given back.

### Mechanism

`pci::claim` called `hw::register_msix(table_phys, 0)`, which mapped a
fresh uncached window onto the MSI-X table (`map_uc`) every time. The
windows are allocated from a bump pointer and never freed, so each
restart of a driver with MSI-X (virtio, xHCI, igb...) left one more kernel
mapping of the device's table, and page tables for them, until reboot.

### Fix

The window belongs to the function, not to the claim: the first claim that
uses MSI-X maps it and keeps it in the function's record
(`PciDevice::msix_window`); every later claim reuses it, and
`register_msix` takes the mapped table. Windows are now bounded by the
devices (one per DMAR unit and per function with MSI-X) and the scenario
shows them unchanged (12 KiB) across the claims.

Unmapping the window when the driver dies was considered and not done:
the interrupt line of the dead driver masks its entry through the window
when it is dropped, which happens in `reap()` with interrupts off, and a
kernel window that is unmapped and handed out again needs a TLB shootdown
on every CPU first, which cannot run with interrupts off or with the PCI
locks a claim holds. A window kept for the device's life needs neither,
and a device is never removed while the system runs.

## 4. Typed-ahead console input survived the reader it was typed for

### Evidence

`test=console_orphan` (new scenario `console.orphan_input`): a
`console_orphan` process says `CONSOLE_ORPHAN_READY`, reads one byte of
the line the runner types (`rm -r everything`) and faults with the rest
unread, as a shell that crashes mid-line; a second one then reads with a
2 s timeout. On develop the second one got the dead reader's input:

```text
console_orphan: received 16 byte(s) typed for the dead reader: m -r everything
assertion `left == right` failed: console: bytes typed for a reader that died reached the next one
```

Any later reader of the console would get it: a relaunched shell would
run the rest of the line as a command. Today init does not relaunch the
console shell (it calls `run_shell` once) and only the shell reads the
console, so the risk was latent: it would open as soon as anything reads
the console after a dead reader.

### Mechanism

The console input ring is global and knows nothing about who reads it:
`console_read` hands whatever is queued to whoever holds `READ` on the
console. A reader that died with input queued left it for the next
reader of the same ring.

### Fix

The ring remembers its reader (the process that called `console_read`
last). When that process finishes, before anyone can see its exit, what
it left unread is discarded and logged (`console: N typed byte(s)
discarded: their reader (process P) is gone`), and so is anything that
arrives before the next process reads (logged likewise). The next reader
becomes the reader before its prompt is flushed, so a line typed in
answer to the new prompt is never taken for the dead reader's. Input
typed at boot, before anyone has read, still goes to the first reader.
ADR 0057 states the rule.

## Audit

An independent review of `30a2146` found no correctness bug, no
reachable kernel panic, no double refund and no input an existing
scenario should get that is now lost. It corrected two claims of the
first version of these documents: init does not relaunch the console
shell (finding 4 was latent, see above), and the reuse distance of a
handle value is about 4 * 10^9 closes only when the table is otherwise
empty, 1.7 * 10^7 when it is otherwise full. It also noted the old
interrupt line masking a newer claim's entry (below) and that a
`dma_alloc` without contiguous memory failed silently, now logged.

## Tests

- `cargo xtask check`; `cargo xtask test --level unit` (new host tests in
  `libs/handles`).
- `cargo xtask test --all --area console,drivers,memory,user,pci`: 65
  scenarios, 0 failed (`console.orphan_input`, `memory.dma_lifetime` and
  `drivers.pci_reap` failed on develop with the reproductions above).
- Full suite on the final commit, with 1 and 4 CPUs (the handle table and
  the DMA path are kernel-wide).

## What remains

- Handle values do come back, after 255-256 * 65535 reuses of their
  position (at least about 1.7 * 10^7 closes in the table); the format
  has 32 bits. Widening it would change the ABI and was not needed.
- The console follows its last reader only: a foreground program that
  reads the console and exits also loses the input it left unread, and
  with two live readers one that exits would discard what the other was
  about to read. No program other than the shell reads the console today
  (ADR 0057, alternatives).
- Found by the audit, not introduced here: an interrupt line of an old
  claim (it has `TRANSFER`, so it can outlive the claim) masks entry 0
  when dropped, which can mask the entry a newer claim programmed. Before
  the fix the separate windows pointed at the same entry, so this is
  unchanged; it needs its own fix (revoke the old line on reclaim, or skip
  the mask when the entry no longer holds the line's vector).

## Second audit (2026-10-27)

An independent audit of `30a2146` and `65f8643`, ordered by the
coordinator, found no high or medium finding (handle values bounded and
never shared by two live handles, stale and forged values refused, table
memory bounded; DMA refund exactly once and only after a charge, no frame
or IOMMU leaf leaked; the MSI-X window serialized under the PCI lock and
never unmapped; the console discard without deadlock and logged without
the bytes; each new test fails with its fix reverted). Followed up:

- ADR 0057 overstated two things: a stale handle fails only until its
  value comes back, and a churning table takes about 10 KiB (positions
  are 32 bytes, plus the free queue), not 8. Corrected.
- Two pre-existing PCI defects it confirmed are not part of this branch
  and get their own incident: an old claim's IRQ line, which can outlive
  the claim (it is transferable), masks MSI-X entry 0 (or disables MSI)
  when dropped, after a newer claim programmed it; and the MSI-X table's
  BAR base is not checked again at a claim, only its capability
  register.
- A live reader can lose unread type-ahead when another process becomes
  the reader and exits: as designed (ADR 0057, "Alternatives"; only the
  shell reads the console today).
- Not covered by a test: the refund on the no-contiguous-memory and
  IOMMU failure paths (only the full-handle-table path is exercised).
