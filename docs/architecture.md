# Arquitectura

La referencia completa es el documento maestro [../AriCodeOS.md](../AriCodeOS.md).
Este fichero resume lo implementado hasta ahora y dónde vive cada pieza.

## Estado: 0.0.4 en curso (0.0.3 + Fases 8 y 9)

```text
OVMF (UEFI)
  -> EFI/BOOT/BOOTX64.EFI            AriBoot        boot/
       GOP, ACPI RSDP, cmdline.txt, kernel.elf, tablas de páginas
       (identidad + mapa físico lineal + kernel), BootInfo v3,
       ExitBootServices, salto
  -> aricodeos/kernel.elf            AriKernel      kernel/
       consola serie + framebuffer (fuente 8x16), logger con niveles,
       informe de BootInfo, GDT/TSS/IDT propias con IST para #DF,
       excepciones con volcado, PMM por bitmap, VMM con tablas propias
       (W^X, mapa físico, pila con guard page), heap con crecimiento
       bajo demanda, PIC enmascarado, LAPIC + timer 100 Hz, interrupciones
       habilitadas, hilos del kernel con cambio de contexto cooperativo,
       ARICODEOS_KERNEL_READY
```

## Crates

```text
boot-protocol/       ABI BootInfo (v2): mapa UEFI, framebuffer, RSDP, kernel, pila, PML4, cmdline
libs/elf/            aricode-elf: parser ELF64 mínimo (no_std, tests de host)
libs/x86-paging/     aricode-x86-paging: tablas de 4 niveles genéricas en FrameAlloc/PhysMap (no_std, tests de host)
libs/cmdline/        aricode-cmdline: `clave` / `clave=valor` sin alloc (no_std, tests de host)
libs/fb-console/     aricode-fb-console: consola de texto sobre framebuffer 32 bpp, fuente Terminus 8x16 (OFL), tests de host
libs/x86-cpu/        aricode-x86-cpu: descriptores GDT/TSS/IDT y tabla de excepciones (no_std, tests de host)
libs/pmm/            aricode-pmm: allocator de frames por bitmap sobre almacenamiento externo (no_std, tests de host)
libs/heap/           aricode-heap: heap first-fit con lista de libres, fusión y cabecera por asignación (no_std, tests de host)
libs/sync/           aricode-sync: IrqLock/SpinLock con política de interrupciones enchufable (no_std, tests de host)
libs/sched/          aricode-sched: política round robin sobre Tid con estados, sueño, bloqueo y salida (no_std + alloc, tests de host)
boot/                ariboot: integra lo anterior con la crate `uefi`
kernel/              arikernel: serial, framebuffer (Surface MMIO), console (log::Log), sync (SingleCpu),
                     arch/x86_64 {gdt, idt, interrupts (stubs.s), exceptions, port, pic, pit, apic},
                     memory/{phys, pmm, vmm, heap}, sync (política IRQ), time,
                     task/{context (switch_context, thread_start), stack, thread, scheduler}, informe, panic, salida QEMU
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

Definido en `boot-protocol/` (`BootInfo`, versión 3, ADR 0009). Al entrar al kernel:

- interrupciones deshabilitadas;
- CR3 apunta a tablas construidas por AriBoot: identidad con páginas de 2 MiB
  (solo para el cargador), mapa físico lineal en `PHYS_MAP_BASE` (RW, NX)
  para toda la memoria física no reservada (mínimo 4 GiB) más el framebuffer,
  y el kernel en `0xFFFF_FFFF_8000_0000` con páginas de 4 KiB y permisos por
  segmento ELF;
- `rsp` en una pila de 64 KiB reservada por el loader, direccionada por el
  mapa físico;
- `rdi` = puntero a `BootInfo` a través del mapa físico; las direcciones que
  contiene son físicas (`phys_map_offset` las traduce);
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

## Memoria física (Fase 5)

- `memory::pmm::init` recorre el mapa UEFI: administra hasta el final de la
  última región no reservada, coloca el bitmap (1 bit por frame de 4 KiB) en
  la región libre más baja con sitio, libera las regiones `Free` y vuelve a
  reservar el primer MiB (frame 0, trampolín SMP futuro) y el propio bitmap.
- Las regiones `Boot` (datos del cargador y de boot services) siguen
  reservadas: se reclamarán cuando el kernel deje de depender del `BootInfo`
  original.
- `test=pmm` ejecuta la autoprueba sobre el bitmap real (unicidad, orden,
  double free, contigüidad alineada) y exige que las estadísticas vuelvan al
  estado inicial.

## Memoria virtual (Fase 6)

`memory::vmm::init` construye las tablas definitivas con frames del PMM y
las activa; a partir de ahí la identidad del cargador deja de existir:

```text
0xFFFF_8000_0000_0000  mapa físico lineal, 2 MiB, RW NX G (RAM no reservada + framebuffer)
0xFFFF_FFFF_7FFE_E000  guard page (sin mapear)
0xFFFF_FFFF_7FFE_F000  pila del kernel, 64 KiB, RW NX G
0xFFFF_FFFF_8000_0000  imagen: .text R-X, .rodata R--, .data/.bss RW- (todo G)
```

- Los límites de sección vienen de los símbolos del linker script; los de fin
  se redondean a página (la sección siguiente empieza en la página siguiente).
- Tras activar las tablas, `kernel_entry` cambia a la pila nueva y continúa
  en `kernel_main`; la pila del cargador queda sin uso.
- Las regiones `Boot` del mapa UEFI (cargador y boot services, ~42 MiB en
  QEMU) se devuelven al PMM en ese momento.
- Un desbordamiento de pila toca la guard page: #PF sin pila válida -> #DF
  en la pila IST; el handler reconoce `rsp`/CR2 dentro de la guard y lo
  anuncia como `STACK OVERFLOW`.
- Fallos provocados: `test=vmm` (autoprueba), `write_rodata`, `exec_data`,
  `stack_overflow`.

## Heap del kernel (Fase 7)

```text
0xFFFF_C000_0000_0000  heap, trozos de 1 MiB mapeados bajo demanda, máximo 64 MiB, RW NX G
```

- `memory::heap` es el `#[global_allocator]`; `alloc` de `core` está
  disponible en todo el kernel (`Vec`, `Box`, `String`, `format!`).
- Cuando una asignación no cabe, el allocator mapea otro trozo con frames del
  PMM a través del VMM y reintenta; si se alcanza el máximo o no hay frames
  devuelve nulo y `try_reserve` falla limpiamente (una asignación normal entra
  en pánico por el `handle_alloc_error` por defecto). El crecimiento es
  transaccional: un fallo a medias desmapea y devuelve lo ya tomado.
- La aritmética de direcciones del allocator es comprobada: en la mitad alta
  del espacio virtual un `Layout` válido de decenas de TiB desbordaría
  `usize` (auditoría `docs/audits/2026-09-26-audit-9d8c7e5.md`).
- `aricode-heap` guarda una cabecera de 16 bytes por asignación con el bloque
  real: `dealloc` no depende del `Layout` y las alineaciones grandes no
  pierden memoria. Los bloques libres se fusionan al liberar.
- `test=heap` ejercita colecciones, crecimiento a más de 4 MiB, fallo
  controlado y ausencia de fugas.

## Sincronización

Todo el estado global (consola de framebuffer, PMM, VMM, heap, GDT/TSS/IDT)
vive tras `sync::IrqLock`, que guarda `RFLAGS.IF`, ejecuta `cli` al tomar y
restaura al soltar: una sección crítica no puede ser interrumpida ni reentrada
por un handler. Con una CPU, encontrar un lock tomado solo puede ser reentrada,
así que `lock` entra en pánico tras muchos giros en vez de colgar. La consola
usa `try_lock` y cae a solo serie si está tomada (una excepción dentro de la
propia consola no bloquea el diagnóstico). Orden de adquisición:
heap -> pmm -> vmm -> console.

## Interrupciones externas y tiempo (Fase 8)

- El 8259 se remapea a 0x20/0x28 y se enmascara por completo; las
  interrupciones llegan por el Local APIC, mapeado sin caché en
  `0xFFFF_D000_0000_0000` y habilitado con vector espurio 0xFF.
- El timer del LAPIC se calibra contra el canal 2 del PIT (10 ms) y se
  programa periódico a `time::TICK_HZ` = 100 Hz en el vector 0x20; en QEMU
  resultan ~62 700 ticks/ms con divisor 16.
- `time` mantiene el contador de ticks, `uptime_ms` y `sleep_ms` (espera con
  `hlt`, sin scheduler todavía). `test=timer` verifica el ritmo contra el PIT.
- Los vectores >= 32 se despachan desde `exceptions::handle` a
  `apic::handle_interrupt`, que envía EOI (salvo el espurio).

## Hilos del kernel (Fase 9)

```text
0xFFFF_E000_0000_0000  pilas de hilo: slot = tid, cada uno guard page (sin mapear) + 32 KiB RW NX G
```

- `aricode-sched::RoundRobin` decide quién sigue; el kernel guarda por hilo
  su `saved_rsp` y su pila. El contexto de arranque se adopta como hilo
  `main` (tid 0) y el idle (tid 1, `hlt` + `yield`) se elige cuando la cola
  está vacía y nunca se encola.
- `switch_context` (System V) guarda rbx, rbp, r12..r15 y rsp; un hilo nuevo
  arranca por `thread_start` con r12 = entrada y r13 = argumento, habilita
  interrupciones y al retornar entra en `exit`.
- El lock del planificador se suelta antes del cambio de contexto, con las
  interrupciones deshabilitadas: el hilo que reanuda nunca hereda un lock.
- Los hilos terminados los recoge el siguiente hilo que reanuda tras un
  cambio (`reap`): nunca se libera la pila en uso. Las guard pages de hilo
  también se reconocen en el diagnóstico de `STACK OVERFLOW`.
- `test=threads`: dos hilos alternan estrictamente con `yield` y 40 workers
  terminan y devuelven sus pilas al PMM.

## Deuda conocida

- Sin preempción todavía: un hilo que no cede monopoliza la CPU (Fase 10).
- Los slots de pila no se reutilizan (uno por tid); el espacio virtual
  reservado permite ~2^31 hilos, así que no es un límite práctico aún.
- El LAPIC también queda cubierto por el mapa físico lineal con caché WB;
  solo se accede por la ventana sin caché, pero es un alias a resolver
  cuando se reserve MMIO fuera del mapa lineal.
- Sin I/O APIC ni MSI: ninguna IRQ de dispositivo todavía (Fase 17).
- El heap nunca devuelve trozos al PMM ni compacta; el slab/size-class de la
  sección 16 llegará cuando haya métricas que lo pidan. Las tablas intermedias
  creadas durante un crecimiento fallido tampoco se devuelven (se reutilizan).
- Las tablas del cargador (tipo `Kernel` en el mapa) no se recuperan: su
  extensión no se conoce con precisión.
- Sin `invlpg`/TLB shootdown genérico: solo el heap y la autoprueba invalidan.
- Sin APIC: las interrupciones externas siguen deshabilitadas (Fase 8).
- El estado de la consola es una celda de una sola CPU sin bloqueo; se
  sustituye por un spinlock consciente de interrupciones con los threads (Fase 9).
- El loader depende de la crate `uefi` (ver ADR 0006).
