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
