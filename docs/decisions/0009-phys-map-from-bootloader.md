# ADR 0009: Linear physical map built by the loader (protocol v3)

- **Date:** 2026-09-26
- **Status:** Accepted

## Context

Up to 0.0.2 the kernel accessed physical memory through the identity map
that AriBoot left active. When it builds its own tables (Phase 6) the
identity map goes away, and every physical pointer in the kernel (the PMM bitmap,
`BootInfo`, cmdline, framebuffer, stack) would have had to be relocated in
the middle of execution.

## Decision

AriBoot maps all non-reserved physical memory (and the framebuffer) at
`PHYS_MAP_BASE = 0xFFFF_8000_0000_0000`, RW and NX, in addition to the identity
map that only it needs. `BootInfo` v3 provides `phys_map_offset` and `phys_map_limit`
and all its addresses are physical; the stack and `BootInfo` itself are
handed over already as physical-map addresses. The kernel uses only
that map from its first instruction, and its VMM reproduces it in the final
tables, so switching CR3 does not invalidate any pointer.

## Alternatives

- Relocating pointers after the table switch: fragile and hard to audit.
- Keeping the identity map in the kernel: breaks W^X on the physical map and exposes
  executable low addresses.

## Consequences

- Physical memory is always accessed with `memory::phys::phys_to_virt`.
- The paging library is generic over `FrameAlloc` and `PhysMap`: the
  loader uses bump + identity, the kernel uses PMM + offset.
- Any MMIO device outside RAM must be mapped explicitly
  (the framebuffer already is).
