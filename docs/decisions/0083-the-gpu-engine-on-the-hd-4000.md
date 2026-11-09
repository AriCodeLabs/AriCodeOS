# ADR 0083: The GPU engine on the HD 4000

- **Date:** 2026-11-07
- **Status:** Accepted, as stage A of ADR 0080. Steps 1 to 4 of the device study are
  done (sources, veterans, read-only probe). The owner chose option (a) of point 1 on
  2026-11-07: the kernel, with the logic in a host-tested library.

## Context

ADR 0080 stage A: a job on the real engine of the reference laptop's HD Graphics 4000
(Ivy Bridge GT2, `8086:0166`), filling and copying with the blitter, a hung batch reset,
the GPU's frequency under control. Its decision 4 says no user-space program may write
the GPU's page tables on a machine without an IOMMU: the kernel writes them, checked
against the memory a driver presents, and "the driver keeps everything else". This ADR
finds out what "everything else" may safely be on this family.

## Sources

- **Intel PRM, Ivy Bridge, 2012**, all volumes. Cited by volume, part and section; the
  licence allows no derivative works, so the code and this record describe it in our own
  words. Volume 1 Part 4 (blitter) documents the ring, the status page, the blitter
  commands and the reset bit; Part 2 the GTT and the per-process GTT (PPGTT).
- **The manual says nothing about forcewake, the GT FIFO, RC6, RPS (frequency) or the
  pcode mailbox.** It only mentions them in passing (Vol 1 Part 4 2.1.9, Part 2 5.10) and
  warns that touching MMIO 0x9500 to 0x97FF can keep the GT out of RC6 (Part 6 1.1). The
  later Haswell register volume, a sibling that documents them, sits behind a filter
  against automated downloads on x.org, which we did not get around. For these registers
  the source is Linux i915, read and compared, and each value it predicts was checked
  against the laptop by the probe below.
- **Linux i915** (MIT, read, not copied): `intel_ring_submission.c`, `gen6_engine_cs.c`,
  `gen6_ppgtt.c`, `intel_ggtt.c`, `intel_reset.c`, `intel_uncore.c`, `intel_rps.c`,
  `intel_rc6.c`, `i915_cmd_parser.c` and the register headers; the commits that bypass the
  gen7 command parser (311a50e7), withdraw secure batches from user space (44157641) and
  add the gen9 blitter parser for CVE-2019-0155; the 2020 revert of full PPGTT on gen7.
- **Genode's GPU multiplexer** (gen8 on): a user-level component owns the GTT, the
  PPGTTs, contexts and fences; clients submit batches in their own PPGTT.
- **Fuchsia's Magma**: a privileged system driver programs the hardware; clients send
  command buffers into a per-connection address space. It runs behind an IOMMU and
  targets gen9 on, so it is not a precedent for this machine.

## What the hardware showed (read-only probe, 2026-11-07)

A temporary kernel (dev build 39, never merged) read 0:2.0's configuration, the always-on
registers, the GT registers once the GT was found awake, the fences and all 524,288 GGTT
entries. Nothing was written.

| Item | Read | Meaning |
|---|---|---|
| PCI | command 0x0007; MSI capability present, off; GMS 64 MiB, GTT 2 MiB | Bus mastering already on |
| GT state | `GT_CORE_STATUS` RC0; RC6 residency 0 | Awake since boot |
| Forcewake | `FORCEWAKE_MT` and legacy `FORCEWAKE` both held, acks set; `ECOBUS` bit 5 set | The firmware left it held, so the GT never sleeps. Multi-threaded forcewake is available |
| RC6 | `RC_CONTROL` 0x88060000 (RC6 and RC6p on, hardware mode) | Enabled but never reached, because of the held forcewake |
| Frequency | `RP_STATE_CAP` RP0 21, RP1 7, RPn 7 (×50 MHz: 1050, 350, 350); `RPNSWREQ` and `RPSTAT1` at 7 | Fixed at 350 MHz. RPS is on (`RP_CONTROL` 0x592) but nobody answers its interrupts, so it never rises. Matches Intel's 350 to 1050 MHz for the i3-3217U |
| Engines | Render, video and blitter rings: `CTL` 0, `START` 0, status pages 0, idle, no faults or errors | Untouched |
| PPGTT | Off on every ring; render `GFX_MODE` 0x2800 | The firmware already set the two bits of Linux's IVB TLB workaround on the render ring |
| Fences | All zero | No tiling set up |
| Interrupts | GT and PM all masked; `PMINTRMSK` 0 | |
| **GGTT** | The first 16,383 entries map stolen memory at 0xDBE00000, contiguous; **the remaining 2 GiB of GGTT address space holds uninitialised memory: about 270,000 entries with the valid bit set point at random physical addresses** | Any GPU access outside the firmware's 64 MiB would be DMA to random memory. Linux clears all unused GGTT space at load; so must we, before anything runs |

Raw dump and decoder: `~/aricode-scratch/gpu-reference/probe/` and
`~/aricode-scratch/laptop/gt-decode.py` on the build host.

## What the study found

1. **On Ivy Bridge a batch's privilege is its address space.** Bit 8 of
   `MI_BATCH_BUFFER_START` selects the PPGTT, and a PPGTT batch is non-secure. A GGTT
   batch is secure, and with the PPGTT off the bit must be 0, so every batch is secure
   (Vol 1 Part 4, `MI_BATCH_BUFFER_START`). A separate privilege bit arrives with
   Haswell.
2. **Commands carry no physical addresses**; every address goes through the GGTT or the
   PPGTT (Vol 1 Part 2 4.6.1). The ways out are therefore: writing GTT entries, page
   directory entries or page tables; moving or disabling the PPGTT (`PP_DIR_BASE`,
   `PP_DCLV`, `GFX_MODE` bit 9); privileged commands (`MI_UPDATE_GTT`, stores and
   register loads with the GGTT bit), which run from the ring and from secure batches;
   and GPU writes into pages the kernel trusts, such as the ring and the status page.
3. **In a non-secure batch, privileged commands do not land.** The manual says they
   become no-ops, or in older text a command error; the write never happens either way.
   Linux's gen7 command parser only adds permissions; a batch that fails it still runs,
   non-secure (commit 311a50e7). Its parser became mandatory only on gen9's blitter,
   whose hardware let non-secure batches write sensitive registers.
4. **MMIO cannot be shared by page.** The blitter's 4 KiB register page holds its ring
   registers, `PP_DIR_BASE`, `PP_DCLV` and `GFX_MODE` beside `BCS_SWCTRL`, which a client
   may set. The rest of the register half holds the display, the PCH, fences, the pcode
   mailbox and the memory controller's mirror.
5. **No read-only GPU mappings on this family.** Neither page table format has a write
   bit (Vol 1 Part 2 2.1.2, 2.2.2); every page mapped for the GPU is writable by it.

So decision 4 holds only if the kernel owns more than the page tables: whoever writes the
ring, or any register on the blitter's page, can rewrite the GTT.

## Decision

1. **Where the engine's owner runs: the kernel (owner's choice, 2026-11-07).** The study leaves almost nothing a
   separate driver could hold without breaking decision 4: the GTT, the PPGTT, the ring,
   the status page, every register of the engine, reset and interrupts must stay out of
   reach of any program. The options:
   - **(a) In the kernel, as a library plus thin glue (recommended)**, as ADR 0079 did for
     the display: `libs/intel-gt` holds the logic, host-tested against a register bank
     built from the manual and the probe; the kernel holds the BAR and calls it. Programs
     reach the engine through a GPU capability. A defect is fixed by a network update
     with A/B rollback, as the kernel is.
   - (b) In a privileged user-space service with all of BAR 0, as Genode does. That is the
     authority decision 4 refused on a machine without an IOMMU.
   - (c) A user-space driver with filtered MMIO and a kernel that parses every ring
     command. Linux's parser is 1,600 lines with a history of bypasses; it makes the
     trusted code larger, not smaller.
2. **The kernel owns** BAR 0 (registers and GTT) and BAR 2, never mapped to a program;
   the GGTT, cleared at start to a scratch page except the firmware's display mapping;
   one PPGTT (page directory in the GTT's own memory, page tables in kernel pages);
   `GFX_MODE`, `PP_DIR_BASE`, `PP_DCLV`, the status page and ring registers and memory;
   TLB invalidation; forcewake; RC6 and frequency; interrupts; reset.
3. **A client gives batches and memory, nothing else.** With the `GPU` object (one client
   at a time on this family, decision 5 of ADR 0080; the first process to map or submit,
   until it ends), ABI 10:
   - `gpu_map(gpu, dma, address)`: a DMA buffer the client holds with `READ|WRITE` (the
     GPU can write every page it sees, point 5 above) enters its PPGTT at `address`, in
     `[gpu::FIRST, 2 GiB)`. The buffer stays allocated while mapped.
   - `gpu_unmap(gpu, address)`: `Busy` until the engine has consumed every command given,
     read from the hardware (`HEAD` equals the ring's tail), never from the status page.
   - `gpu_submit(gpu, ptr, len) -> number`: see "Decision, added" below: the kernel
     copies, checks and runs its own copy of the batch.
   - `gpu_wait(gpu, number, timeout_ms)`: `Hung` when no batch has finished for
     `gpu.hang_ms` (2 s by default) with work pending; the blitter is then reset, the ring
     restarted with the PPGTT, and the batches given until then are lost.
   A process that ends while it is the client leaves a mark; the next GPU call resets the
   engine if it still works for it, points its pages back at the scratch page and lets
   its buffers go.
4. **The GPU's frequency is a parameter** (no hardcoding, ADR 0080): the floor, 350 MHz,
   by default (`gpu.mhz` at boot); a program asks for another with `gpu_frequency`, within
   the limits and `gpu.max_mhz`; the `gpu` thread holds the lowest while the CPU package is
   at `gpu.hot_c`. RPS is on as Linux has it, its interrupts quiet. See "The GPU's clock".
5. **Forcewake is taken and released by the kernel** around its own register writes; the
   firmware's held bits are cleared at start, as Linux does, so RC6 can work.
6. **The way back:** `gpu=off` in `cmdline.txt` leaves the GT as the firmware left it.
   An unknown model, or any state the probe did not see (a ring already running, PPGTT
   on, forcewake that will not answer), is left alone and reported in `diag`.

## Measured before trusting the hardware

From a non-secure batch, before any client relied on it: a register load into the
blitter's page (the manual's "22XX" note in Vol 1 Part 4, 2.1.12, is ambiguous; Linux
allows only `BCS_SWCTRL` and the timestamps), a store with the global GTT bit set, and a
store by index from a flush.

## Measured on the laptop: non-secure batches reach the blitter's registers

Dev build 40 (2026-11-07) ran `gpu_probe` with the design above, batches executed
straight from the client's memory. One non-secure batch in the PPGTT:

| Command in the batch | Aimed at | Result |
|---|---|---|
| `MI_LOAD_REGISTER_IMM` | the blitter's watchdog threshold, 0x2217C | **Written**: 0x00150000 became 0x00AB0000 |
| `MI_FLUSH_DW`, store by index | dword 0x90 of the global status page | **Written**: 0 became 0xBAD00002 |
| `MI_STORE_DATA_IMM` with the global GTT bit | the same dword | Unknown: the flush wrote it after |

So on Ivy Bridge the hardware does not keep a non-secure batch away from the
blitter's own register page, which the manual's note on "22XX" registers (Vol 1
Part 4, 2.1.12) meant. That page holds `PP_DIR_BASE`, `PP_DIR_DCLV` and the PPGTT's
enable bit. A batch could move the page directory onto GTT entries whose pages it can
write (the status page, through the store above), and from there reach any physical
page: decision 4 would not hold. Linux does not protect against this on gen7: its
command parser runs a batch that fails it anyway, non-secure, "leaving it to the HW to
validate" (`intel_engine_cmd_parser`), and its gen7 PPGTT aliases the global GTT. The
laptop went back to dev build 38 at once; nothing of this reached `develop`.

**Decision, added:** the kernel checks every batch, against a short list, and runs
its own copy of it.

- `gpu_submit` takes the batch from the client's memory, copies it into a page of the
  kernel's (one of four), checks the copy, and runs the copy, non-secure, from a range
  of the PPGTT below `gpu::FIRST` that no mapping can reach. The client cannot change
  what was checked.
- Allowed: `MI_NOOP`; `MI_BATCH_BUFFER_END`; `MI_FLUSH_DW` with no store; linear
  `XY_COLOR_BLT` and `XY_SRC_COPY_BLT` at 32 bits a pixel whose every byte, source and
  destination, lies inside one of the client's mappings; and `MI_BATCH_BUFFER_START`
  to the copy's own start, which can only loop (the reset ends it) and lets the reset
  be tested on any machine. Everything else is refused, the batch is not run, and the
  refusal is recorded with the dword it stopped at.
- With no store reaching the status page, its sequence numbers are the kernel's again;
  idleness is still read from the hardware.

This is a check of a handful of commands, not Linux's parser of every command and
register: what is not on the list does not run. Whether the global GTT store lands
stays unmeasured: the check refuses it before the hardware sees it.

## Stage A on the laptop (dev build 42, 2026-11-07)

`run gpu_probe`, two surfaces of 1024x768 at 32 bits (3 MiB each), eight runs of each,
GPU at its lowest clock, 350 MHz; each GPU time includes the submit and the wait:

| Work | GPU best (median) | CPU best (median) |
|---|---|---|
| Fill 3 MiB | 971 us (972), 3.2 GB/s | 269 us (276), 11.7 GB/s |
| Copy 3 MiB | 1598 us (1610), 2.0 GB/s | 536 us (547), 5.9 GB/s |

- Every pixel was checked after each GPU fill and copy: all right, so the PPGTT, the LLC
  coherence between the GPU's writes and the CPU's reads, and the completion number work.
- The four batches that must not run (the register load, the store by index, the global
  GTT store, a fill one row past the client's memory) were each refused, and the canary
  and the watchdog register kept their values.
- A batch jumping to its own start was caught and reset after 2,012 ms (limit 2,000), and
  the engine filled correctly after it.
- Dev build 41 had sampled the engine's head to find hangs, as Linux did before 5.5: a
  batch looping on itself keeps the head moving, and it was not caught. The rule became
  Linux's heartbeat, above. The status page is trustworthy for it once no checked batch
  can write it, and a batch of the checked commands ends well under a second.
- **At 350 MHz the blitter is a third of the CPU's speed** on these sizes. The figures at
  the GPU's other clocks, and the heat they cost, are the next measurement; the point of
  stage A was the path, not the speed. Temperature during the runs: 57 to 58 C.

## The resources this device offers

| Resource | Today | Stage A |
|---|---|---|
| Blitter engine | Used: fill and copy | Tiled surfaces, more blit types as needed |
| Render engine, EUs | Not used | Stage B |
| Video engine | Not used | Not planned |
| PPGTT | Used: one, for the current client | |
| RC6 | Forcewake released at boot, so the GT may enter it | Residency to read in `diag` |
| Frequency | 350 MHz, `gpu.mhz` at boot | Measured at each clock with the heat; a call to set it |
| Interrupts (MSI) | Off: waits spin 2 ms, then poll each millisecond | `MI_USER_INTERRUPT` is already in each request |
| Fences, tiling | Unused | With tiled surfaces |
| Hardware watchdog (`BCS_CTR_THRSH`) | Unused: the heartbeat rule replaces it | |

## Order of work

1. `libs/intel-gt`: register model, forcewake, GGTT clear, PPGTT, ring bring-up,
   completion, reset, frequency, the batch check, against a fake bank with the probe's
   state, including every "leave it alone" case. Done; each test was seen to fail on a
   deliberately broken build.
2. Kernel glue and the GPU capability. Done.
3. On the laptop: the measurements, fill and copy beside the CPU, a looping batch reset.
   Done; the frequency at other clocks is next.
4. Independent audit of the kernel part. Done: `docs/audits/2026-11-07-gpu-stage-a.md`.

## Consequences

- The kernel grows by the engine's owner, of the order of Genode's multiplexer without
  contexts; the code that decides lives in a host-tested library.
- A program with the GPU capability can hang the blitter (reset covers it), and write
  only memory it presented with write rights.
- QEMU emulates no Intel GPU: host tests and the laptop are the only coverage, as for
  ADR 0079. `gpu.absent` covers a machine without one: the `GPU` object exists, every
  call says `NotFound`, `diag` says nothing.
- The kernel now reads every batch, which this ADR first meant to avoid: the laptop showed
  the hardware alone does not hold the line on this family. The check is a list of five
  commands; a client needing more gets them added one by one, each with its limits.
- Any application may ask for `gpu` in its manifest: through it a program reaches only
  its own buffers, and one program at a time. **The owner's choice (2026-11-07), after
  audit L8:** no narrowing by signature now, since every program that can run today is
  signed (the system image or a package verified against the release key) and narrowing
  would change nothing. When applications from other publishers arrive (their own ADR,
  as ADR 0073 left them out), `gpu` becomes a permission the owner grants to each
  application at install and can take back, not one the manifest alone obtains. What
  limits heavy programs (video editing, training models) today is elsewhere: the GPU only
  fills and copies until stages B and C, and an application's memory is capped at 8 MiB.
- **Audit** (`docs/audits/2026-11-07-gpu-stage-a.md`): no way past the batch check was
  found. One High and four Medium fixed: memory a stuck engine may reach is kept for good,
  never freed; the screen must be covered to its end and the kernel's GPU pages sit past
  the CPU's aperture; a bring-up failing halfway stops the engine and keeps its pages; every
  page table exists from boot; at most 256 mappings. Seven Low fixed. The eighth, that any
  application may ask for `gpu`, stays: it reaches only its own buffers, one program at a
  time, and narrowing it to signed packages is the owner's call.
- Dev build 43 on the laptop after the fixes: the same figures, every refusal, the hang
  reset after 2,011 ms; the client released by the `gpu` thread when it ended.

## The GPU's clock (2026-11-09)

`gpu_frequency(gpu, mhz)` (ABI 12) asks for the clock, granted within the GPU's limits and
`gpu.max_mhz`; the `gpu` thread drops it to the lowest while the CPU package, which carries
the GPU too, is at `gpu.hot_c` (85 C by default) until 5 degrees below; the next client
starts at the boot's clock.

**Measured first, wrong:** asking with RPS off (`RP_CONTROL` 0), as the study inferred was
safe, left the GPU at 350 MHz whatever was asked (dev build 46). Linux asks with RPS on in
hardware normal mode (`rps_set_power`) and writes 0 only to turn RPS off for good; with its
interrupts masked here, only a request moves the clock. With that (dev build 47), each run
`gpu_probe`'s 3 MiB surfaces, eight runs, best time:

| Clock | Fill | Copy | CPU package after |
|---|---|---|---|
| 350 MHz | 712 us, 4.4 GB/s | 1,344 us, 2.3 GB/s | 57 C |
| 650 MHz | 385 us, 8.2 GB/s | 780 us, 4.0 GB/s | 57 C |
| 1050 MHz | 241 us, 13.1 GB/s | 598 us, 5.3 GB/s | 58 C |
| The CPU | 273 us, 11.5 GB/s | 575 us, 5.5 GB/s | |

At its highest clock the blitter fills faster than the CPU and copies as fast, while the
CPU is free; copies are bound by memory, which both share. These bursts are too short to
heat the laptop; the heat brake's decision is host-tested, and it has not yet been seen to
act on the laptop.

