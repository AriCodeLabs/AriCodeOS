# ADR 0006: Using the `uefi` crate in AriBoot

- **Date:** 2026-09-26
- **Status:** Accepted

## Context

Writing the UEFI tables and protocols by hand (boot services, GOP, SimpleFileSystem,
LoadedImage, config tables) means hundreds of lines of `unsafe` FFI that already
exist, audited, in the `uefi` crate (rust-osdev).

## Decision

AriBoot depends on `uefi` 0.41 (features `alloc`, `global_allocator`,
`panic_handler`, `logger`). The ELF parser, the page table construction
and the jump to the kernel are our own code.

## Alternatives

- Our own FFI: more control, much more `unsafe` to audit, no benefit for M0.

## Consequences

- The bootloader is not part of the kernel: the dependency does not enter the
  runtime TCB (it disappears after `ExitBootServices`).
- If a 100% in-house chain is wanted in the future, replacing the crate is a
  change local to `boot/`.
