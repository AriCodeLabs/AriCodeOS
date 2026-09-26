# ADR 0002: Arranque exclusivamente UEFI

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Contexto

Todo el hardware objetivo (QEMU/OVMF y el portátil de pruebas) arranca por UEFI.

## Decisión

No se implementa BIOS legacy, MBR, real mode ni la transición 16 -> 32 -> 64
bits. AriBoot es una aplicación UEFI (`BOOTX64.EFI`, PE32+).

## Consecuencias

- Se parte de un entorno de 64 bits con paginación activa y GOP disponible.
- El mapa de memoria, ACPI y el framebuffer se obtienen del firmware.
- Hardware sin UEFI queda fuera del alcance.
