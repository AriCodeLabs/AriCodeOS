# ADR 0084: Memory for big programs

- **Date:** 2026-11-07
- **Status:** Accepted. The owner chose the ceiling per application: half the usable RAM,
  as a parameter.

## Context

A program's memory beyond its 256 KiB heap arena is DMA buffers: physically contiguous, at
most 8 MiB each (`DMA_MAX_LEN`), and the manifest's `memory`, the quota on them, is at most
8 MiB too. A video editor or a program training models needs hundreds of MiB, and ADR 0080
(section 5, stage F) already named the missing piece: memory that is not one contiguous run.
Contiguous memory of that size is not found once the machine has run a while; the limit is
not a constant to raise.

What exists to build on: zero-fill pages backed on first touch, with a kill and a clear
reason when memory runs out (ADR 0049); objects that live while any handle or mapping holds
them (ADR 0038); a quota per process set by its launcher from the manifest.

## What others do, and where they failed

- **Linux**: anonymous `mmap`, backed on first touch, and by default it promises more than
  it has (overcommit, heuristic mode). When the promise cannot be kept the OOM killer picks
  a process by score, often not the one that asked: years of tuning knobs
  (`oom_score_adj`, `vm.overcommit_memory=2`) exist to work around it.
- **Windows**: `VirtualAlloc` with `MEM_COMMIT` charges the commit limit (RAM plus page
  file) at reservation; pages are still taken on first touch. A commit that cannot be kept
  fails at the call, where the program can handle it; nothing is killed later.
- **Zircon**: a VMO is a memory object of pages, committed on fault or on request, mapped
  into address spaces with `zx_vmar_map`, pinned for devices with a BTI. Its job limits
  bound a process's memory.
- **seL4**: user space retypes untyped memory itself; the right model for a verified
  kernel, a large change for us.

## Decision

1. **A memory object (`MEM`, ABI 11).** `mem_create(len) -> handle` makes `len` bytes
   (whole pages) of zeroed memory, not contiguous, with `READ|WRITE|DUPLICATE|TRANSFER`.
   No capability is needed: the quota bounds it. It is mapped with `mmio_map`, as a DMA
   buffer is, and unmapped with `mmio_unmap`; it lives while any handle or mapping holds it
   (ADR 0038). `dma_phys` refuses it: it is not for devices.
2. **Pages on first touch, charged at creation** (Windows' commit, not Linux's
   overcommit). Creating the object charges its whole size to the process's quota and
   promises its pages out of the machine's free memory; each page is taken, zeroed, when
   first touched, from the promise. Other allocations for programs (DMA buffers, segment
   pages on first touch) never take promised frames, so a page of a `MEM` object is always
   there when touched. A promise the machine cannot make fails `mem_create` with `Limit`:
   no process is killed later for memory it was given.
3. **One quota for everything a program holds:** the manifest's `memory` bounds DMA
   buffers and `MEM` objects together, as now for DMA. A manifest may ask for up to
   64 GiB; the kernel grants at most **half the usable RAM** (`mem.app_max_pct=50`, the
   owner's choice), so one application always leaves room for the system and for another.
4. **The runtime's heap grows by itself:** when its arena is full, it creates a `MEM`
   object for the request (at least 1 MiB), maps it and adds it to the heap. A program
   allocates as it would anywhere, within its quota.
5. **The GPU maps it** (ADR 0083): `gpu_map` takes a `MEM` object as it takes a DMA buffer;
   its pages are backed first and stay while mapped.

## Consequences

- A program's memory is bounded by its quota and the machine, not by contiguity: a model
  or a video timeline can live in one object or many.
- DMA buffers stay for devices and window buffers, still contiguous and at most 8 MiB each.
- The promise is the machine's free memory at creation: memory the kernel later needs for
  itself still comes from its reserve (ADR 0049), and an allocation of the kernel's own can
  still find the machine full, as before.
- `diag` shows each process's quota use as now.

## Measured

- **QEMU** (`memory.big`, 512 MiB): making a 64 MiB object took no frame; touching 1,024
  of its pages took 1,056 frames (the pages and 32 page tables); letting it go gave all
  back but those 32 tables, which the process's address range keeps for its next mapping
  (ADR 0038); a call wrote into a page nobody had touched; the heap grew to 40 MiB, five
  times the old limit; 300 MiB was refused though the manifest asked for 1 GiB, since half
  of QEMU's memory is about 240 MiB. With the ceiling removed on purpose, the scenario
  fails at that step.
- **The laptop** (dev build 44): `gpu_probe` filled and copied into a memory object of
  scattered pages mapped for the GPU, with every pixel right and the same figures as with
  a DMA buffer.

## Audit

`docs/audits/2026-11-08-memory-objects.md`: three High, two Medium and four Low, all fixed.
What changed in the design:

- **Page tables are part of the promise's guarantee.** A process's page tables never take
  promised frames or the kernel's reserve, and a memory object's mapping prepares its tables
  when it is made; a promise leaves the kernel 16 MiB or 1/64 of the machine of headroom.
  `memory.full` gives one program all of QEMU's memory and touches every page of it.
- **Nothing a program does can fill the kernel's heap:** an object's page table lives in
  frames of its own, and a process holds at most 512 mappings.
- **The system's own services** (no quota) hold at most `mem.service_max_mib`, 64 MiB, in
  memory objects.
- **Large objects are freed in the background**, in batches, so no work proportional to an
  object's size runs with interrupts off.
