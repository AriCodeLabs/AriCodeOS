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
