# ADR 0009: Mapa físico lineal construido por el cargador (protocolo v3)

- **Fecha:** 2026-09-26
- **Estado:** Aceptada

## Contexto

Hasta 0.0.2 el kernel accedía a la memoria física por el mapa de identidad
que AriBoot dejaba activo. Al construir sus propias tablas (Fase 6) la
identidad desaparece y todos los punteros físicos del kernel (bitmap del PMM,
`BootInfo`, cmdline, framebuffer, pila) habrían tenido que reubicarse en
mitad de la ejecución.

## Decisión

AriBoot mapea toda la memoria física no reservada (y el framebuffer) en
`PHYS_MAP_BASE = 0xFFFF_8000_0000_0000`, RW y NX, además de la identidad que
solo él necesita. `BootInfo` v3 entrega `phys_map_offset` y `phys_map_limit`
y todas sus direcciones son físicas; la pila y el propio `BootInfo` se
entregan ya como direcciones del mapa físico. El kernel usa exclusivamente
ese mapa desde su primera instrucción, y su VMM lo reproduce en las tablas
definitivas, de modo que el cambio de CR3 no invalida ningún puntero.

## Alternativas

- Reubicar punteros tras el cambio de tablas: frágil y difícil de auditar.
- Mantener la identidad en el kernel: rompe W^X en el mapa físico y expone
  direcciones bajas ejecutables.

## Consecuencias

- La memoria física se accede siempre con `memory::phys::phys_to_virt`.
- La librería de paginación es genérica en `FrameAlloc` y `PhysMap`: el
  cargador usa bump + identidad, el kernel usa PMM + desplazamiento.
- Cualquier dispositivo MMIO fuera de la RAM debe mapearse explícitamente
  (el framebuffer ya lo está).
