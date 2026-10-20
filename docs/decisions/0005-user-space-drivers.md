# ADR 0005: User-space drivers

- **Date:** 2026-09-26
- **Status:** Accepted

## Decision

Every driver that can technically live outside the kernel will do so, as an
isolated process with specific hardware capabilities. In QEMU we start with virtio.

## Consequences

- Requires efficient IPC and a driver API (Phase 16).
- The IOMMU is an important security feature (Phase 33).
- First real hardware limited to a defined "Reference Machine".
- A driver that dies leaves its devices without bus mastering before its
  memory is freed, and a function that advertises FLR is reset before the
  next claim; a CPU mapping keeps a DMA buffer alive (ADR 0038, incident
  0004).
