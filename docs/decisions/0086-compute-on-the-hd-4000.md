# ADR 0086: Compute on the HD 4000, without a compiler

- **Date:** 2026-11-09
- **Status:** Accepted as stage B of ADR 0080, on the owner's "ok, do it" (2026-11-08). The
  design point below that changes ADR 0083 follows from the study; it is the kernel's to keep
  decision 4, not a new choice.

## Context

Stage A (ADR 0083) runs the blitter: the kernel owns the GTT, the client's PPGTT, the ring and
every register, and runs its own checked copy of each client batch, from pages it maps in the
client's PPGTT below `gpu::FIRST`. Stage B runs the render engine's GPGPU pipe with
hand-assembled EU kernels, up to a matrix multiply measured against the CPU.

A read-only study (Intel's Ivy Bridge PRM Vol 1 Parts 2, 3 and 7, Vol 2 Part 2, Vol 4 Parts 1
to 3; Linux i915 `gen7_renderclear.c`, `intel_workarounds.c`, `intel_clock_gating.c`,
`i915_cmd_parser.c`; Mesa `elk`, `genxml`, `intel_l3_config.c`; igt `gpgpu_fill`, all MIT) and
a read-only probe of the laptop (dev build 55) found:

1. **The render ring needs no context image** for one client: `MI_SET_CONTEXT` only saves and
   restores state (PRM Vol 1 Part 2, 5.8.1). Its bring-up is the blitter's with base 0x2000,
   status page 0x4080 and reset bit 1 of `GDRST`.
2. **The firmware applies none of Linux's Ivy Bridge render and L3 settings** (the probe:
   L3 control 0x8C47FF80 where Linux writes 0x3C47FF8C, `L3SQCREG4` bit 27 set, `ROW_CHICKEN2`,
   `GT_MODE`, `CACHE_MODE_1`, `UCGCTL2` without Linux's bits). The kernel applies them.
3. **A dispatch** is about 60 dwords: flushes, `PIPELINE_SELECT`, `STATE_BASE_ADDRESS`,
   `MEDIA_VFE_STATE`, `MEDIA_CURBE_LOAD`, `MEDIA_INTERFACE_DESCRIPTOR_LOAD`, `GPGPU_WALKER`,
   `MEDIA_STATE_FLUSH`, a completing `PIPE_CONTROL`. Every pointer it carries is a PPGTT address
   or an offset from one; only commands (never the state in memory) can reach registers, the
   global GTT or MMIO: `PIPE_CONTROL`'s register write and GGTT bits, the walker's indirect
   bit, the VFE gateway's MMIO access bits, and the MI commands.
4. **EU threads write wherever their code computes**, in the whole of the client's PPGTT, and
   every page there is writable by the GPU (ADR 0083, point 5). So once EUs run, nothing the
   kernel trusts may be mapped in a PPGTT the EUs use: **the batch copies of stage A, mapped
   below `gpu::FIRST`, could be rewritten after the check.**
5. **Residue between clients** (CVE-2019-14615): EU registers, L3 and the shared local memory
   keep what the last client left. Linux clears registers and L3 before a change of context
   (`gen7_renderclear.c`), at a cost of up to 40 % on Haswell per switch (Linux issue 1858);
   nothing in the sources shows it clearing the shared local memory.

## Decision

1. **The kernel writes every command; clients describe work.** No client batch runs on either
   engine any more:
   - compute: `gpu_dispatch(gpu, &Dispatch)`, a descriptor of the state bases and bounds, the
     interface descriptor and CURBE offsets, the VFE sizes and the walker's geometry. The
     kernel emits the commands into the render ring (in the global GTT, never in a PPGTT),
     forcing what may reach registers off: gateway MMIO 0, walker indirect and predicate 0,
     `PIPE_CONTROL`s of its own. The state structures and the EU code stay in the client's
     memory, unchecked: no field in them can name anything outside the PPGTT;
   - blits: `gpu_submit` keeps its list and its check, but the kernel writes the checked blits
     into the blitter's ring itself; the copy pages leave the PPGTT.
   Health checks on the descriptor (bases inside the client's mappings, thread counts, URB
   limits, non-zero dimensions) are for the client's sake: security does not rest on them.
2. **Render bring-up** as the blitter's, plus Linux's Ivy Bridge GT, render, context and clock
   gating settings (study tables), re-sent from the ring before each dispatch since there is no
   context image to keep them across RC6; L3 as Mesa's no-SLM configuration first.
3. **Residue cleared at a change of client only** (one client at a time, ADR 0080 decision 5):
   a kernel-built dispatch that zeroes every thread's registers and, with SLM on, the shared
   local memory; an L3 flush and invalidate; the scratch page zeroed. Its time is measured.
4. **A render hang** resets the render domain (`GDRST` bit 1), re-applies the settings, and the
   blitter's TLB mode bits, which live in the render engine's mode register.
5. **EU code is assembled on the host side** by `libs/gen7-eu`, tested bit for bit against
   igt's and Linux's published kernels.

## Order of work

B0 probe (done). B1 host: per-engine ring, render settings, `Dispatch` and its emitter, the
assembler. B2 render ring with an empty request and a reset. B3 a first dispatch writing each
group's id. B4 blits written by the kernel, copies out of the PPGTT; EU writes outside the
client's mappings land on the scratch page. B5 an endless EU loop caught and reset. B6 the
residue clear, checked by a second client. B7 SGEMM against the CPU at each clock, with the
heat. B8 audit.

## Progress

- **B0** (dev build 55, read only): the render ring off and idle, the PPGTT off, the
  firmware's L3 and render settings not Linux's (raw dump on the build host,
  `~/aricode-scratch/gpu-reference/stage-b/`).
- **B1** (host): `intel-gt`'s rings work on either engine; Linux's settings are tables
  named after its workarounds, applied through MMIO or loaded from the ring; the render
  engine's `PIPE_CONTROL`s and request are encoded as Linux emits them. 34 host tests,
  each change of behaviour seen to fail them.
- **B2** (dev build 56, 2026-11-09): with Linux's settings applied at boot, the render
  ring started; an empty request finished in 3 us; the render domain was reset, its
  settings applied again and its ring started again; a second empty request finished.
  `gpu_probe` then passed whole on the blitter (fill, copy, clocks, every refusal, the
  hang reset): neither the settings nor the render reset disturbed it. `diag` says
  `render ring ready (3 us)`.
- **B3** (dev builds 57 and 58, 2026-11-09): `gpu_dispatch` (ABI 13) takes a
  `gpu::Dispatch`; `intel-gt`'s `compute` checks it and writes igt's gen7 GPGPU commands
  between Linux's gen7 flushes, gateway MMIO, walker indirect and predicate forced off
  (host tests against igt's `gen7_gpgpu_fillfunc`, each forced bit seen to fail them).
  The kernel tracks each engine's numbers, losses and heartbeat apart; a render reset
  applies its settings again. On the laptop, two kernels assembled by `gen7-eu` ran on
  256 thread groups of one SIMD16 thread: igt's fill, every byte right, in 142 us from
  call to wait; one writing each group's number, every byte right, in 159 us. Four bad
  descriptors were refused, the status page's canary and the blitter's watchdog
  register did not change, and the blitter's probe passed whole before them. Dev 57's
  probe expected a wait on a number far behind the last to be refused; numbers compare
  with wrap-around on both engines, so it reads as done: the probe now waits on one far
  ahead (dev 58). Until B4, a client's EU threads can write the blitter's checked batch
  copies: the branch is not merged.
- **B4** (dev build 59, 2026-11-09): the check returns the commands before
  `MI_BATCH_BUFFER_END`, and the kernel writes them into the blitter's ring between its
  TLB flush and its closing stores; `MI_BATCH_BUFFER_START` is refused (ABI 13), and the
  copy pages are gone from the PPGTT and from memory. On the laptop the blits address the
  client's PPGTT from the ring as the PRM says (fill and copy checked pixel by pixel, at
  the same speeds as from a batch: fill 972 us at 350 MHz, 504 us at 1050), and a jump
  is refused with the four batches of ADR 0083.
- **B5** (dev build 59): the hang test is now an EU thread that jumps to itself. The
  heartbeat caught it and reset the render domain after 2117 ms (`gpu.hang_ms` 2000);
  then an EU fill and a blitter fill were both right. Every engine's number, loss and
  heartbeat are its own.
- **B6** (dev builds 60 and 61, 2026-11-09): the residue clear. Linux's runs 128 media
  objects whose threads wait in a loop so that, it hopes, every slot gets one, and does not
  check. Ours, `intel-gt`'s `clear`, runs a GPGPU group as large as a half-slice holds on each
  half-slice (64 threads twice on this GT2); every thread zeroes g1 to g126, writes Linux's
  512 bytes through the render cache, and marks its slot (half-slice, EU and thread, from
  `sr0`) with the clear's number, so the kernel counts the slots it reached. It runs when a
  client that dispatched EU work goes, from pages mapped in the PPGTT only meanwhile; the
  scratch page is zeroed; a failed clear, or the shared local memory on (checked at boot and
  at each clear: the firmware leaves it off and nothing a client runs can turn it on), leaves
  the render engine off. On the laptop: a pattern written into every register by one client
  was read back by its next threads on 128 of 128 slots (the residue is real here), and
  still after 5 s idle (the GT sleeping does not clear it); the clear reached 128 of 128
  slots in 68 us; a second client then saw the pattern on none of 128. Dev 60's clear took
  its number from the CURBE and marked 2 slots: each thread of a group gets its own section
  of the CURBE (PRM Vol 2 Part 2, 1.3.3), so the number is now an immediate in code
  assembled for each clear. Not cleared, as in Linux: the architecture registers
  (accumulators, flags, address).
- **B7** (dev builds 62 to 65, 2026-11-09): `run gpu_probe 3` multiplies two 1024 x 1024
  float matrices on the EUs, each thread an 8 x 16 tile of C (media block reads of A's 8 x 8
  and B's 8 x 16 a step, 128 `mad(8)`), with eighths as inputs so every sum is exact: the
  GPU's product equals the CPU's to the bit. The CPU (one core of the i3-3217U, the loop in
  i-k-j order) took 422 ms, 5.1 GFLOP/s. The GPU, best of 10 runs: 600 ms (3.6 GFLOP/s) at
  350 MHz, 420 ms (5.1) at 650, 320 to 340 ms (6.3 to 6.7) at 1050, the package at 63, 67
  and 76 to 78 C after each clock's runs. That is about 2 % of the GT2's peak: this kernel
  is the correct first one, not a fast one. Two variants measured slower and were dropped:
  fetching the next step while multiplying (380 ms at 1050 MHz), and the walker's X picking
  tile rows, so the threads in flight share B's strip (460 ms). The bottleneck is not one
  thread's latency nor the DRAM traffic modelled; finding it is a study of its own (Intel's
  and others' SGEMM work on gen7, larger register tiles, L3 use), before the next change.
- **B8** (2026-11-09): an independent read-only audit of B3 to B6
  (`docs/audits/2026-11-09-gpu-compute.md`) found no High issue; its two Medium and three
  Low findings were fixed in `b8c25df` and checked on the laptop (dev build 64). Open: the
  L3 cache and a freed, reused frame are not measured yet.
