# Arquitectura

La referencia completa es el documento maestro [../AriCodeOS.md](../AriCodeOS.md).
Este fichero resume lo implementado hasta ahora y dónde vive cada pieza.

## Estado: M0 First Boot

```text
OVMF (UEFI)
  -> EFI/BOOT/BOOTX64.EFI            AriBoot        boot/
       GOP, ACPI RSDP, kernel.elf, tablas de páginas, BootInfo v1,
       ExitBootServices, salto
  -> aricodeos/kernel.elf            AriKernel      kernel/
       serie COM1, framebuffer, informe de BootInfo,
       ARICODEOS_KERNEL_READY, panic handler
```

## Contrato AriBoot -> AriKernel

Definido en `boot-protocol/` (`BootInfo`, versión 1). Al entrar al kernel:

- interrupciones deshabilitadas;
- CR3 apunta a tablas construidas por AriBoot: identidad con páginas de 2 MiB
  para toda la memoria física no reservada (mínimo 4 GiB) y el kernel en
  `0xFFFF_FFFF_8000_0000` con páginas de 4 KiB y permisos por segmento ELF
  (NX fuera de `.text`, solo lectura fuera de `.data`/`.bss`);
- `rsp` en una pila de 64 KiB reservada por el loader;
- `rdi` = puntero físico a `BootInfo`;
- EFER.NXE activado.

Toda la memoria que el kernel debe conservar (imagen, pila, tablas, BootInfo)
está marcada en el mapa UEFI con el tipo `0x8000_0000` (`MemoryRegionKind::Kernel`).

## Deuda conocida de M0

- La identidad física es RWX; W^X en el mapa físico llega con el VMM (Fase 6).
- El kernel no tiene GDT/IDT propias todavía: usa las del firmware (Fase 4).
- Sin consola de texto en framebuffer, solo serie (Fase 3).
- El loader depende de la crate `uefi` (ver ADR 0006).
