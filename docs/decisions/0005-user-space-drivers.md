# ADR 0005: Drivers en espacio de usuario

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Decisión

Todo driver que técnicamente pueda vivir fuera del kernel lo hará, como proceso
aislado con capabilities de hardware concretas. En QEMU se empieza por virtio.

## Consecuencias

- Requiere IPC eficiente y una API de driver (Fase 16).
- El IOMMU es una característica de seguridad importante (Fase 33).
- Primer hardware real limitado a una "Reference Machine" definida.
