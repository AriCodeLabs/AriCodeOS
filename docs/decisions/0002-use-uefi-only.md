# ADR 0002: UEFI-only boot

- **Date:** 2026-09-26
- **Status:** Accepted

## Context

All target hardware (QEMU/OVMF and the test laptop) boots through UEFI.

## Decision

Legacy BIOS, MBR, real mode and the 16 -> 32 -> 64-bit transition are not
implemented. AriBoot is a UEFI application (`BOOTX64.EFI`, PE32+).

## Consequences

- We start from a 64-bit environment with paging enabled and GOP available.
- The memory map, ACPI and the framebuffer are obtained from the firmware.
- Hardware without UEFI is out of scope.
