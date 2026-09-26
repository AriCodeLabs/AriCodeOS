# ADR 0004: Seguridad basada en capabilities

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Decisión

Los recursos son objetos kernel accesibles solo mediante handles/capabilities
que codifican objeto, derechos, delegabilidad y lifetime. Identidad no implica
acceso; una aplicación arranca sin privilegios.

## Consecuencias

- Las syscalls operan sobre handles, no sobre identificadores globales.
- Los drivers reciben solo MMIO, IRQ y DMA autorizados.
- La identidad humana (usuarios) se separa de la autoridad (capabilities).
