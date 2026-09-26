# Arquitectura

La referencia completa es el documento maestro [../AriCodeOS.md](../AriCodeOS.md).
Este fichero resume lo implementado hasta ahora y dónde vive cada pieza.

## Estado: 0.0.2 (M0 + Fase 3 + Fase 4)

```text
OVMF (UEFI)
  -> EFI/BOOT/BOOTX64.EFI            AriBoot        boot/
       GOP, ACPI RSDP, cmdline.txt, kernel.elf, tablas de páginas,
       BootInfo v2, ExitBootServices, salto
  -> aricodeos/kernel.elf            AriKernel      kernel/
       consola serie + framebuffer (fuente 8x16), logger con niveles,
       informe de BootInfo, GDT/TSS/IDT propias con IST para #DF,
       excepciones con volcado, ARICODEOS_KERNEL_READY, panic handler
```

## Crates

```text
boot-protocol/       ABI BootInfo (v2): mapa UEFI, framebuffer, RSDP, kernel, pila, PML4, cmdline
libs/elf/            aricode-elf: parser ELF64 mínimo (no_std, tests de host)
libs/x86-paging/     aricode-x86-paging: tablas de 4 niveles sobre un pool (no_std, tests de host)
libs/cmdline/        aricode-cmdline: `clave` / `clave=valor` sin alloc (no_std, tests de host)
libs/fb-console/     aricode-fb-console: consola de texto sobre framebuffer 32 bpp, fuente Terminus 8x16 (OFL), tests de host
libs/x86-cpu/        aricode-x86-cpu: descriptores GDT/TSS/IDT y tabla de excepciones (no_std, tests de host)
boot/                ariboot: integra lo anterior con la crate `uefi`
kernel/              arikernel: serial, framebuffer (Surface MMIO), console (log::Log), sync (SingleCpu),
                     arch/x86_64 {gdt, idt, interrupts (stubs.s), exceptions}, informe, panic, salida QEMU
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

## Consola y logging (Fase 3)

- `print!`/`println!` escriben texto sin formato en serie y framebuffer
  (banner y marcadores como `ARICODEOS_KERNEL_READY`).
- Las macros de la crate `log` (`trace!`..`error!`) pasan por `console::KernelLogger`,
  que antepone `[LEVEL]` y escribe en ambas salidas. Nivel por defecto `info`;
  `log=<nivel>` en la cmdline lo cambia y un valor inválido produce un `WARN`.
- El panic handler escribe solo por serie (`serial_println!`): no toca estado
  compartido, por si el panic ocurre a medio escribir en el framebuffer.
- La consola de framebuffer mantiene una copia de las celdas y repinta al
  hacer scroll; nunca lee de VRAM.

## CPU (Fase 4)

- GDT propia: nulo, código ring 0 (0x08), datos ring 0 (0x10), TSS (0x18).
  Los descriptores se codifican en `aricode-x86-cpu` (probados contra valores
  conocidos) y `arch::x86_64::gdt` los carga y recarga CS con `retfq`.
- TSS con IST1 apuntando a una pila de 16 KiB dedicada a #DF, de modo que un
  double fault con la pila corrupta sigue produciendo diagnóstico.
- IDT con los 256 vectores apuntando a stubs generados (ADR 0008). Vectores
  con código de error y sin él se normalizan en el mismo `InterruptFrame`.
- Excepciones: #BP avisa y continúa; el resto vuelca registros, y para #PF
  CR2, tipo de acceso, modo y causa, todo por serie, y termina en pánico.
  Vectores >= 32 se registran como inesperados (no hay APIC todavía).
- Fallos provocados por cmdline: `test=breakpoint|invalid_opcode|page_fault|double_fault`.

## Deuda conocida

- La identidad física es RWX; W^X en el mapa físico llega con el VMM (Fase 6).
- Sin APIC: las interrupciones externas siguen deshabilitadas (Fase 8).
- El estado de la consola es una celda de una sola CPU sin bloqueo; se
  sustituye por un spinlock consciente de interrupciones con los threads (Fase 9).
- El loader depende de la crate `uefi` (ver ADR 0006).
