# ADR 0004: Capability-based security

- **Date:** 2026-09-26
- **Status:** Accepted

## Decision

Resources are kernel objects reachable only through handles/capabilities
that encode object, rights, delegability and lifetime. Identity does not imply
access; an application starts without privileges.

## Consequences

- Syscalls operate on handles, not on global identifiers.
- Drivers receive only authorised MMIO, IRQ and DMA.
- Human identity (users) is separated from authority (capabilities).
