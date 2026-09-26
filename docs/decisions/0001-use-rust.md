# ADR 0001: Rust `no_std` como lenguaje del kernel (frente a AriCode)

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Contexto

El proyecto dispone de un compilador propio, AriCode (`~/git-proyect/aricoderoot`),
y había que decidir si el kernel se escribía con él o con Rust. La valoración
completa está en la sección 7.4 del documento maestro.

## Decisión

Kernel, bootloader, drivers y servicios base se escriben en Rust `no_std`.
AriCode no forma parte del Trusted Computing Base.

## Alternativas

- **AriCode.** Descartado en su estado actual: único target `x86_64-linux`,
  prólogo con syscalls Linux, sin punteros crudos, `unsafe`, asm, FFI, layout
  controlado ni objetos enlazables. Hacerlo viable equivale a escribir un
  compilador de sistemas nuevo.
- **C.** Descartado como lenguaje principal por ausencia de memory safety.

## Consecuencias

- Toolchain vía rustup con targets `x86_64-unknown-none` y `x86_64-unknown-uefi`.
- AriCode se integrará encima del kernel como carga de trabajo de usuario
  (sección 105 del documento maestro).
- Revisar si AriCode incorpora backend freestanding y FFI reales.
