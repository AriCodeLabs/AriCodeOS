# ADR 0008: Solo Rust estable; stubs de interrupción en ensamblador generado

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Contexto

Los handlers de excepción en Rust suelen escribirse con la ABI
`extern "x86-interrupt"`, que sigue siendo inestable (requiere nightly).
El proyecto fija la toolchain en `rust-toolchain.toml` y quiere builds
reproducibles sin depender de un nightly concreto.

## Decisión

AriCodeOS compila únicamente con Rust estable. Los 256 stubs de interrupción y
el trampolín común son ensamblador (`kernel/src/arch/x86_64/stubs.s`)
incluido con `global_asm!`, generado por `tools/gen-stubs.py` y versionado.
La lógica de despacho está en Rust (`interrupt_dispatch`) y recibe un
`InterruptFrame` con layout `repr(C)` que refleja exactamente lo que el
trampolín empuja.

## Alternativas

- `extern "x86-interrupt"` en nightly: menos código, pero acopla el proyecto
  al canal inestable y a cambios de la ABI.
- Funciones `naked` (estables desde 1.88): viable, pero 256 funciones Rust
  con `asm!` cada una son menos legibles que una tabla generada.

## Consecuencias

- Cualquier cambio en el marco de pila se hace en dos sitios: el generador y
  `InterruptFrame`. Los escenarios del área `interrupts` detectan desajustes.
- Las estructuras de la arquitectura (descriptores, TSS, IDT) viven en
  `libs/x86-cpu` y se prueban en host; el kernel solo ejecuta las
  instrucciones privilegiadas.
