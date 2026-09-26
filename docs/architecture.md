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

## Crates

```text
boot-protocol/       ABI BootInfo (v2): mapa UEFI, framebuffer, RSDP, kernel, pila, PML4, cmdline
libs/elf/            aricode-elf: parser ELF64 mínimo (no_std, tests de host)
libs/x86-paging/     aricode-x86-paging: tablas de 4 niveles sobre un pool (no_std, tests de host)
libs/cmdline/        aricode-cmdline: `clave` / `clave=valor` sin alloc (no_std, tests de host)
boot/                ariboot: integra lo anterior con la crate `uefi`
kernel/              arikernel: serial, framebuffer, informe, panic, salida QEMU
tools/xtask/         build, image, qemu, scenario, test, check
tests/qemu/          escenarios QEMU declarativos (ver tests/README.md)
```

Regla: la lógica que se pueda probar en host vive en `libs/*`; `boot/` y
`kernel/` solo integran.

Invariantes de seguridad de memoria en el cargador (auditoría 2026-09-26,
`docs/audits/`):

- `aricode-elf` rechaza cualquier ELF cuyo rango virtual no sea representable,
  con segmentos solapados o cuya entrada no esté en un segmento ejecutable, y
  descarta los `PT_LOAD` vacíos para que no participen en el mapeo. Nada se
  reserva ni se copia hasta que el ELF completo ha pasado la validación.
- `aricode-x86-paging` solo desreferencia tablas que su propio `PagePool` ha
  repartido; la única operación `unsafe` para el llamador es tomar posesión de
  la región del pool.

## Contrato AriBoot -> AriKernel

Definido en `boot-protocol/` (`BootInfo`, versión 2). Al entrar al kernel:

- interrupciones deshabilitadas;
- CR3 apunta a tablas construidas por AriBoot: identidad con páginas de 2 MiB
  para toda la memoria física no reservada (mínimo 4 GiB) y el kernel en
  `0xFFFF_FFFF_8000_0000` con páginas de 4 KiB y permisos por segmento ELF
  (NX fuera de `.text`, solo lectura fuera de `.data`/`.bss`);
- `rsp` en una pila de 64 KiB reservada por el loader;
- `rdi` = puntero físico a `BootInfo`;
- EFER.NXE activado;
- si existía `\aricodeos\cmdline.txt`, su contenido está copiado en memoria
  de kernel y referenciado por `cmdline_address/size`.

Toda la memoria que el kernel debe conservar (imagen, pila, tablas, BootInfo)
está marcada en el mapa UEFI con el tipo `0x8000_0000` (`MemoryRegionKind::Kernel`).

## Deuda conocida de M0

- La identidad física es RWX; W^X en el mapa físico llega con el VMM (Fase 6).
- El kernel no tiene GDT/IDT propias todavía: usa las del firmware (Fase 4).
- Sin consola de texto en framebuffer, solo serie (Fase 3).
- El loader depende de la crate `uefi` (ver ADR 0006).
