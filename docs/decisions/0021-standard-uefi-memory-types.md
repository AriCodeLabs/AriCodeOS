# ADR 0021: AriBoot reserves memory with standard UEFI memory types

- **Date:** 2026-10-10
- **Status:** Accepted

## Context

Up to 0.8.1 AriBoot marked every page the kernel had to keep (image,
stack, `BootInfo`, tables, initrd, cmdline) with a custom memory type from
the OS loader range (`0x8000_0000`, `MemoryRegionKind::Kernel`), and the
kernel simply reclaimed the rest of the boot memory (`Boot`).

On the first physical test (Phase 34, test laptop) AriBoot stops at the
first call to `GetMemoryMap`, right after those reservations. The `uefi`
crate warns in `MemoryType::custom` that some firmwares hang or misbehave
with custom types (OSDev: "My bootloader hangs if I use user defined
EFI_MEMORY_TYPE values"); Linux and GRUB only use standard types. In
addition, `uefi::boot::memory_map` chains several calls with `assert!`s
and no tracing, so on hardware there is no way to tell which one it stops
at.

## Decision

1. **Standard types only.** AriBoot reserves all its pages as
   `LOADER_DATA`. `MEMORY_TYPE_KERNEL` and `MemoryRegionKind::Kernel` are
   removed; in the map those pages are `Boot`.
2. **BootInfo v4** adds `page_tables_base`, `page_tables_size` and
   `boot_info_address`, and `BootInfo::kernel_ranges()` returns the ranges
   the kernel must keep: image, initial stack, the `BootInfo` page, initrd
   and cmdline. AriBoot's page tables are not included: the kernel
   reclaims them when it activates its own, which it could not do before
   because it did not know their extent.
3. **Reclaim with explicit re-reservation.** `vmm::reclaim_boot_memory`
   frees the `Boot` regions (the loader's reservations may be merged with
   other firmware `LOADER_DATA` in the same descriptor) and then reserves
   `kernel_ranges()` again. The trace says how much was returned and how
   much was kept.
4. **Memory map with direct, traced calls.** `boot/src/memmap.rs` calls
   `GetMemoryMap` twice (size and read), logging status and sizes before
   and after, validates the response instead of using `assert!`, and walks
   the descriptors with the `boot-protocol` iterator. `ExitBootServices`
   still uses the crate.

## Consequences

- The kernel image, its stack and the `BootInfo` live in `Boot` memory
  until the kernel classifies it; any code that reclaims boot memory must
  go through `reclaim_boot_memory` and respect `kernel_ranges()`.
- The kernel's map has one type fewer; `report_memory` and the
  `boot-protocol` tests are adjusted. The protocol is incompatible with v3
  kernels.
- AriBoot's page tables (tens of KiB) go back to the PMM.
- `docs/hardware.md` describes how to read the `memory:` traces and the
  beacons if a firmware stops at the memory map.
