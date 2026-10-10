# ADR 0021: AriBoot reserva con tipos de memoria UEFI estándar

- **Fecha:** 2026-10-10
- **Estado:** Aceptada

## Contexto

Hasta la 0.8.1 AriBoot marcaba todas las páginas que el kernel debía
conservar (imagen, pila, `BootInfo`, tablas, initrd, cmdline) con un tipo de
memoria propio del rango OS loader (`0x8000_0000`, `MemoryRegionKind::Kernel`),
y el kernel recuperaba sin más el resto de la memoria de arranque (`Boot`).

En la primera prueba física (Fase 34, portátil de pruebas) AriBoot se para
en la primera llamada a `GetMemoryMap`, justo después de esas reservas. La
crate `uefi` avisa en `MemoryType::custom` de que algunos firmwares se
cuelgan o se comportan mal con tipos propios (OSDev: "My bootloader hangs if
I use user defined EFI_MEMORY_TYPE values"); Linux y GRUB solo usan tipos
estándar. Además, `uefi::boot::memory_map` encadena varias llamadas con
`assert!`s y sin trazas, así que en hardware no se sabe en cuál se para.

## Decisión

1. **Solo tipos estándar.** AriBoot reserva todas sus páginas como
   `LOADER_DATA`. `MEMORY_TYPE_KERNEL` y `MemoryRegionKind::Kernel`
   desaparecen; en el mapa esas páginas son `Boot`.
2. **BootInfo v4** añade `page_tables_base`, `page_tables_size` y
   `boot_info_address`, y `BootInfo::kernel_ranges()` devuelve los rangos que
   el kernel debe conservar: imagen, pila inicial, página del `BootInfo`,
   initrd y cmdline. Las tablas de AriBoot no están: el kernel las recupera
   al activar las suyas, cosa que antes no podía hacer porque no conocía su
   extensión.
3. **Recuperación con reserva explícita.** `vmm::reclaim_boot_memory`
   libera las regiones `Boot` (las reservas del cargador pueden quedar
   fusionadas con otras `LOADER_DATA` del firmware en un mismo descriptor) y
   después vuelve a reservar `kernel_ranges()`. La traza dice cuánto se
   devolvió y cuánto se conservó.
4. **Mapa de memoria con llamadas directas y trazadas.** `boot/src/memmap.rs`
   llama a `GetMemoryMap` dos veces (tamaño y lectura) anotando estado y
   tamaños antes y después, valida la respuesta en vez de `assert!`, y recorre
   los descriptores con el iterador de `boot-protocol`. `ExitBootServices`
   sigue usando la crate.

## Consecuencias

- La imagen del kernel, su pila y el `BootInfo` viven en memoria `Boot`
  hasta que el kernel la clasifica; cualquier código que recupere memoria de
  arranque debe pasar por `reclaim_boot_memory` y respetar `kernel_ranges()`.
- El mapa del kernel tiene un tipo menos; `report_memory` y los tests de
  `boot-protocol` se ajustan. Protocolo incompatible con kernels v3.
- Las tablas de AriBoot (decenas de KiB) vuelven al PMM.
- `docs/hardware.md` describe cómo leer las trazas `memoria:` y las balizas
  si un firmware se para en el mapa de memoria.
