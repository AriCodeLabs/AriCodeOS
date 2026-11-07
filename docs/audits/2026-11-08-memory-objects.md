# Audit: memory objects (ADR 0084), 2026-11-08

Independent, read-only review of commit 8e2e9ea (`feature/big-memory`). The properties under
attack: no process reaches a frame it does not own or one freed; a promise is always kept;
the quota cannot be bypassed; nothing panics the kernel or holds interrupts off for long.

**Result:** three High, two Medium and four Low findings; all fixed in `9438ee2`, with the
thread self-test's expectation brought up to date in `8d81a40`. Verified by the full suite
with 1 and 4 CPUs (224 scenarios each), `memory.big` and the new `memory.full`, and on the
laptop (dev build 45).

| # | Finding | Fix |
|---|---|---|
| H1 | Page tables for memory-object mappings, the kernel heap, kernel stacks and IOMMU tables did not respect promises; a fully promised machine could fail a touch and drain the kernel's reserve | Process page tables come from `UserFrames`, which never take the reserve or promised frames; a memory object's mapping prepares its page tables when made, so a first touch needs only its promised page; a promise leaves the kernel 16 MiB or 1/64 of the machine of headroom. `memory.full` promises all of QEMU's memory to one program and touches every page |
| H2 | Kernel metadata no quota pays (per-page tables in the kernel heap, unbounded mappings, the GPU's list of frames) could exhaust the 64 MiB kernel heap | An object's page table lives in frames of its own; at most 512 foreign mappings per process (`memory.big` hits it at 511); `gpu_map` checks sizes before taking pages and takes them one at a time |
| H3 | Services without a quota (most of init's) got unlimited memory objects, and their heaps now grow by them | They hold at most `mem.service_max_mib` (64) |
| M1 | Freeing, unmapping and flushing work proportional to an object's size ran with interrupts off | Objects over 8 MiB are freed by a `reclaim` thread in batches of 512 frames; unmapping walks only present tables; a flush over 64 pages reloads CR3 |
| M2 | Mapped memory objects were not counted against the mapper's quota | They are, as DMA buffers are (ADR 0038) |
| L1 | Frame numbers are 32-bit | Frames from 16 TiB up are refused |
| L2 | `dma_alloc` could drain the kernel's reserve | It keeps it |
| L3 | The heap could not grow under a small quota | It falls back to the exact size |
| L4 | `region_len` refused memory objects; promised memory was invisible | It takes them; `sysinfo::MEMORY_PROMISED` and a line in `diag` |
