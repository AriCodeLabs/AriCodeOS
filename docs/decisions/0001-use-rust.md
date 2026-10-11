# ADR 0001: Rust `no_std` as the kernel language (versus AriCode)

- **Date:** 2026-09-26
- **Status:** Accepted

## Context

The project has its own compiler, AriCode (`~/git-proyect/aricoderoot`),
and we had to decide whether the kernel would be written with it or with Rust.
The full assessment is in section 7.4 of the master document.

## Decision

The kernel, bootloader, drivers and base services are written in Rust `no_std`.
AriCode is not part of the Trusted Computing Base.

## Alternatives

- **AriCode.** Rejected in its current state: single target `x86_64-linux`,
  prologue with Linux syscalls, no raw pointers, `unsafe`, asm, FFI, controlled
  layout or linkable objects. Making it viable amounts to writing a new
  systems compiler.
- **C.** Rejected as the main language because it lacks memory safety.

## Consequences

- Toolchain via rustup with the `x86_64-unknown-none` and `x86_64-unknown-uefi` targets.
- AriCode will be integrated on top of the kernel as a user workload
  (section 105 of the master document).
- Revisit if AriCode gains a real freestanding backend and FFI.
