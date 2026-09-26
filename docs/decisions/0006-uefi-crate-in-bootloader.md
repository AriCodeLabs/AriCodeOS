# ADR 0006: Uso de la crate `uefi` en AriBoot

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Contexto

Escribir a mano las tablas y protocolos UEFI (boot services, GOP, SimpleFileSystem,
LoadedImage, config tables) supone cientos de líneas de FFI `unsafe` que ya
existen, auditadas, en la crate `uefi` (rust-osdev).

## Decisión

AriBoot depende de `uefi` 0.41 (features `alloc`, `global_allocator`,
`panic_handler`, `logger`). El parser ELF, la construcción de tablas de páginas
y el salto al kernel son código propio.

## Alternativas

- FFI propio: más control, mucho más `unsafe` que auditar, sin beneficio para M0.

## Consecuencias

- El bootloader no forma parte del kernel: la dependencia no entra en el TCB
  en ejecución (desaparece tras `ExitBootServices`).
- Si en el futuro se quiere una cadena 100 % propia, sustituir la crate es un
  cambio local a `boot/`.
