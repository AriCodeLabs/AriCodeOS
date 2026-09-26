# Arquitectura

La referencia completa es el documento maestro [../AriCodeOS.md](../AriCodeOS.md).
Este fichero resume lo implementado hasta ahora y dónde vive cada pieza.

## Estado: 0.0.8 (0.0.7 + Fases 18 a 21)

```text
OVMF (UEFI)
  -> EFI/BOOT/BOOTX64.EFI            AriBoot        boot/
       GOP, ACPI RSDP, cmdline.txt, kernel.elf, initrd.img, tablas de
       páginas (identidad + mapa físico lineal + kernel), BootInfo v3,
       ExitBootServices, salto
  -> aricodeos/kernel.elf            AriKernel      kernel/
       consola serie + framebuffer (fuente 8x16), logger con niveles,
       informe de BootInfo, GDT/TSS/IDT propias con IST para #DF,
       excepciones con volcado, PMM por bitmap, VMM con tablas propias
       (W^X, mapa físico, pila con guard page), heap con crecimiento
       bajo demanda, PIC enmascarado, LAPIC + timer 100 Hz, interrupciones
       habilitadas, hilos del kernel, scheduler round robin preemptivo con
       sueño y colas de espera, procesos en ring 3 con espacio propio,
       syscalls, tablas de handles con derechos y revocación por objeto,
       IPC por endpoints con transferencia de capabilities y timeouts,
       initrd con init y programas de usuario, init lanza y supervisa
       procesos con la capacidad SPAWNER, ACPI + I/O APIC, drivers en ring 3
       con capabilities IRQ/MMIO/DMA, enumeración PCIe por ECAM, init reclama
       el virtio-blk (BARs, MSI-X) y lanza su driver en ring 3 como servicio
       de bloques por IPC (disco GPT), fsd monta AriFS v0 sobre él y sirve
       ficheros por IPC; con `shell` en la cmdline init lanza arish, que lee
       la consola serie y apaga o reinicia por `system_power`;
       ARICODEOS_KERNEL_READY
```

## Crates

```text
boot-protocol/       ABI BootInfo (v3): mapa UEFI, framebuffer, RSDP, kernel, pila, PML4, cmdline, mapa físico
libs/elf/            aricode-elf: parser ELF64 mínimo (no_std, tests de host)
libs/x86-paging/     aricode-x86-paging: tablas de 4 niveles genéricas en FrameAlloc/PhysMap (no_std, tests de host)
libs/cmdline/        aricode-cmdline: `clave` / `clave=valor` sin alloc (no_std, tests de host)
libs/fb-console/     aricode-fb-console: consola de texto sobre framebuffer 32 bpp, fuente Terminus 8x16 (OFL), tests de host
libs/x86-cpu/        aricode-x86-cpu: descriptores GDT/TSS/IDT y tabla de excepciones (no_std, tests de host)
libs/pmm/            aricode-pmm: allocator de frames por bitmap sobre almacenamiento externo (no_std, tests de host)
libs/heap/           aricode-heap: heap first-fit con lista de libres, fusión y cabecera por asignación (no_std, tests de host)
libs/sync/           aricode-sync: IrqLock/SpinLock con política de interrupciones enchufable (no_std, tests de host)
libs/sched/          aricode-sched: política round robin sobre Tid con estados, sueño, bloqueo y salida (no_std + alloc, tests de host)
libs/handles/        aricode-handles: tabla de handles con generación y derechos, duplicación solo reductora (no_std + alloc, tests de host)
libs/initrd/         aricode-initrd: formato del initrd, lector sin copia (no_std) y escritor (feature `write`, xtask); tests de host
libs/acpi/           aricode-acpi: RSDP, XSDT/RSDT, MADT (LAPIC, I/O APIC, overrides), MCFG; sin AML (no_std + alloc, tests de host)
libs/pci/            aricode-pci: direccionamiento ECAM, cabecera de configuración, BARs, capabilities, clases (no_std + alloc, tests de host)
libs/virtio/         aricode-virtio: virtqueue split sobre DMA, estructuras virtio-pci modernas, peticiones virtio-blk (no_std, tests de host)
libs/gpt/            aricode-gpt: CRC32, cabecera y entradas GPT, escritor para el disco de pruebas (no_std, tests de host)
libs/arifs/          aricode-arifs: AriFS v0, directorio plano con tabla de ficheros de capacidad fija sobre `BlockDevice` (no_std, tests de host)
sdk/proto/           aricode-proto: protocolos IPC versionados de los servicios (bloques v1, ficheros v1) (no_std, tests de host)
sdk/abi/             aricode-abi: números de syscall, errores, handles iniciales, derechos y tipos de objeto (no_std, tests de host)
sdk/rust/            aricode-sys: wrappers de syscall para programas de usuario (no_std, sin alloc), clientes de los servicios de bloques y ficheros, y `user.ld`, el linker script de todo programa de usuario
user/testprogs/      programas de prueba de ring 3 (Rust y ensamblador) enlazados como ELF estáticos en 0x400000
services/init/       AriInit: primer proceso; recibe SPAWNER y la raíz PCI, lanza/supervisa servicios y reclama dispositivos para sus drivers
services/virtio-blk/ driver virtio-blk en ring 3 y servicio de bloques por IPC (interfaz moderna, cola 0 en DMA, MSI-X, GPT)
services/arifs/      fsd: servicio de ficheros AriFS en ring 3 sobre el servicio de bloques
services/shell/      arish: shell interactivo en ring 3 (help, ps, mem, ls, cat, devices, services, uptime, reboot, shutdown, exit)
boot/                ariboot: integra lo anterior con la crate `uefi`
kernel/              arikernel: serial, framebuffer (Surface MMIO), console (log::Log con IrqLock y try_lock),
                     arch/x86_64 {gdt, idt, interrupts (stubs.s), exceptions, port, pic, pit, apic, ioapic, msr, syscall, usermode},
                     acpi (localización de tablas), pci (ECAM, enumeración, reclamación con sondeo de BARs y MSI-X),
                     hw (IrqLine por GSI o MSI-X, Region, DmaBuffer, ventanas sin caché, registro de vectores, banco de pruebas),
                     object (KernelObject revocable, tabla inicial), ipc (Endpoint, Message, benchmark), initrd,
                     user/{process (cargador ELF, copy_from/to_user, espera de salida), programs (initrd), syscalls},
                     memory/{phys, pmm, vmm, heap}, sync (política IRQ), time,
                     task/{context (switch_context, thread_start), stack, thread, scheduler, wait}, informe, panic, salida QEMU
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
- `aricode-x86-paging` es genérica en dos proveedores: `FrameAlloc` entrega
  frames de 4 KiB (bump sobre páginas UEFI en el cargador, PMM en el kernel) y
  `PhysMap` traduce física a puntero (identidad en el cargador, mapa lineal en
  el kernel). `PhysMap` es un trait `unsafe`: quien lo implementa garantiza que
  la traducción es válida para todo frame que el allocador pueda entregar. El
  `Mapper` desreferencia las tablas que creó con su `FrameAlloc` y, tras
  `share_kernel_half` (también `unsafe`), las tablas del kernel que copia por
  referencia: el llamador garantiza que ese PML4 origen es válido y que sus
  PDPT viven tanto como el espacio que los comparte (siempre, en el caso del
  kernel).

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

- GDT propia con el layout que exigen `syscall`/`sysret`
  (`aricode-x86-cpu::layout`, única referencia): nulo, código ring 0 (0x08),
  datos ring 0 (0x10), datos usuario (0x18, selector 0x1B con RPL 3), código
  usuario (0x20, selector 0x23), TSS (0x28, 16 bytes). Los descriptores se
  codifican en `aricode-x86-cpu` (probados contra valores conocidos) y
  `arch::x86_64::gdt` los carga, recarga CS con `retfq` y comprueba el layout.
- TSS con IST1 apuntando a una pila de 16 KiB dedicada a #DF, de modo que un
  double fault con la pila corrupta sigue produciendo diagnóstico.
- IDT con los 256 vectores apuntando a stubs generados (ADR 0008). Vectores
  con código de error y sin él se normalizan en el mismo `InterruptFrame`.
- Excepciones: #BP avisa y continúa; una excepción con `CS.RPL = 3` termina
  el proceso de usuario (ver Modo usuario); el resto vuelca registros, y para
  #PF CR2, tipo de acceso, modo y causa, todo por serie, y termina en pánico.
  Los vectores >= 32 se despachan al APIC (ver Interrupciones externas).
- Fallos provocados por cmdline: `test=breakpoint|invalid_opcode|page_fault|double_fault`.

## Memoria física (Fase 5)

- `memory::pmm::init` recorre el mapa UEFI: administra hasta el final de la
  última región no reservada, coloca el bitmap (1 bit por frame de 4 KiB) en
  la región libre más baja con sitio, libera las regiones `Free` y vuelve a
  reservar el primer MiB (frame 0, trampolín SMP futuro) y el propio bitmap.
- Las regiones `Boot` (datos del cargador y de boot services) quedan
  reservadas en este punto; el VMM las devuelve al PMM al activar las tablas
  propias (ver Memoria virtual).
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

- `memory::heap` es el `#[global_allocator]`; la crate `alloc` (`Vec`, `Box`,
  `String`, `format!`) está disponible en todo el kernel. `core::alloc` solo
  aporta las interfaces `Layout` y `GlobalAlloc`.
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

Todo el estado global (consola de framebuffer, PMM, VMM, heap, GDT/TSS/IDT,
planificador, tabla de procesos, colas de espera) vive tras `sync::IrqLock`,
que guarda `RFLAGS.IF`, ejecuta `cli` al tomar y restaura al soltar: una
sección crítica no puede ser interrumpida ni reentrada por un handler. Con una
CPU, encontrar un lock tomado solo puede ser reentrada, así que `lock` entra
en pánico tras muchos giros en vez de colgar. La consola usa `try_lock` y cae
a solo serie si está tomada (una excepción dentro de la propia consola no
bloquea el diagnóstico).

Anidamientos reales (un lock exterior se mantiene mientras se toma el
interior); cualquier otro par se toma en secuencia, nunca anidado:

```text
VMM -> PMM            vmm::with mantiene el VMM y el Mapper pide frames al PMM (PmmFrames)
heap.mapped -> VMM    grow mantiene el lock de crecimiento y mapea (y, por lo anterior, llega al PMM)
heap.mapped -> PMM    grow pide y devuelve frames
PROCESSES -> SCHED    process::wait consulta el estado del hilo con la tabla tomada
WaitQueue.waiters     hoja: se suelta antes de bloquear o despertar
heap.heap, console    hoja: nunca llaman a otro subsistema
```

Prohibiciones que se derivan: nunca entrar al VMM con el PMM tomado (el
mapper podría necesitar una tabla y reentrar en el PMM); nunca tomar la
tabla de procesos desde dentro del planificador (`reap` suelta el
planificador antes de liberar un espacio de usuario); el lock del
planificador se suelta siempre antes de un cambio de contexto. El lock de
crecimiento del heap (`mapped`) es distinto del lock del allocator (`heap`):
una asignación toma el segundo, un crecimiento el primero y luego el
segundo brevemente para añadir la región.

## Interrupciones externas y tiempo (Fase 8)

- El 8259 se remapea a 0x20/0x28 y se enmascara por completo; las
  interrupciones llegan por el Local APIC, mapeado sin caché en
  `0xFFFF_D000_0000_0000` y habilitado con vector espurio 0xFF.
- El timer del LAPIC se calibra contra el canal 2 del PIT (10 ms) y se
  programa periódico a `time::TICK_HZ` = 100 Hz en el vector 0x20; en QEMU
  resultan ~62 700 ticks/ms con divisor 16.
- `time` mantiene el contador de ticks y `uptime_ms`; `time::sleep_ms` delega
  en el planificador (ver Scheduler preemptivo). `test=timer` verifica el
  ritmo contra el PIT.
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

## Scheduler preemptivo (Fase 10)

- El handler del timer, tras el EOI, llama a `scheduler::on_tick`: despierta
  a los hilos cuyo tick ha llegado y, si el quantum (2 ticks = 20 ms) ha
  expirado o alguien despertó, preempta con la misma ruta de cambio que
  `yield`. El hilo preemptado queda con su marco de interrupción en su pila y
  reanuda por el `iretq` del handler.
- Ningún lock del kernel puede ser preemptado: todos deshabilitan
  interrupciones. El lock del planificador se suelta antes del cambio.
- `scheduler::sleep_ms` duerme sin consumir CPU; `time::sleep_ms` delega en él.
- `task::wait::WaitQueue` implementa bloqueo con condición evaluada bajo su
  lock y `wake_one`/`wake_all`; un despertar que llega antes de bloquearse no
  se pierde porque `unblock` falla y el hilo reevalúa.
- `test=sched`: tres hilos que nunca ceden reparten la CPU (mínimo >= mitad
  del máximo), un hilo duerme 50 ms con una duración observada de 5 a 7 ticks
  (50 a 70 ms en total), y dos consumidores se despiertan uno a uno y luego
  todos.

## Modo usuario (Fase 11)

- GDT con el layout que exige `syscall`/`sysret` (`aricode-x86-cpu::layout`):
  código y datos de kernel, datos y código de usuario (DPL 3), TSS. `STAR`,
  `LSTAR` y `FMASK` se programan en `arch::x86_64::syscall::init`.
- Cada proceso tiene su PML4: la mitad alta se copia del kernel (256 PDPT
  reservados en el arranque y compartidos, de modo que los mapeos que el
  kernel añada después, como ventanas MMIO, se ven en todos los espacios) y la baja contiene su código en `0x40_0000` (solo lectura y
  ejecución) y su pila en `0x7F_C000..0x80_0000` (RW, NX). El bit USER se
  propaga por toda la ruta de tablas.
- El hilo principal del proceso es un hilo del kernel normal cuyo `cr3` es el
  del proceso; arranca en ring 0, fija nada más y salta a ring 3 con `iretq`.
  En cada cambio de contexto el planificador carga el CR3 del hilo siguiente
  si difiere y fija su pila de kernel en `TSS.rsp0` y en la entrada `syscall`.
- `syscall_entry` guarda el `rsp` de usuario, carga la pila de kernel del
  hilo, construye un `SyscallFrame`, habilita interrupciones y despacha en
  Rust; antes de `sysretq` las deshabilita. Convención en `sdk/abi`: `rax`
  número, `rdi rsi rdx r10 r8 r9` argumentos, resultado en `rax`, negativo
  `-4095..=-1` = error. Syscalls actuales: `process_exit`, `console_write`,
  `thread_yield`, `thread_sleep`, `thread_get_tid`, `abi_version`,
  `handle_close`, `handle_duplicate`, `object_kind`, `object_id`,
  `object_revoke`, `endpoint_create`, `ipc_send`, `ipc_receive`,
  `process_spawn` (con transferencia de handles al hijo), `process_wait`,
  `irq_wait`, `mmio_map`, `region_len`, `dma_alloc`, `dma_phys`, `pci_count`,
  `pci_info`, `pci_claim`, `console_read`, `system_info`, `system_power`.
- Los punteros de usuario se validan página a página contra las tablas del
  proceso (`copy_from_user`): un puntero de kernel devuelve `Fault`, nunca se
  desreferencia.
- Una excepción con `CS.RPL = 3` no es un pánico: se diagnostica por serie y
  el proceso termina (`Killed`). Al recoger el hilo se libera todo el
  espacio de usuario, tablas incluidas.
- Programas de prueba en `user/testprogs` (crate aparte): binarios en
  ensamblador (`hello`, `kernel_read`, `kernel_write`, `privileged`,
  `bad_syscall`, `bad_pointer`) y en Rust sobre `aricode-sys`
  (`sleep_yield`, `handles`, `caps`, `ipc_server`, `ipc_client`). `cargo
  xtask` los compila como ELF estáticos para `x86_64-unknown-none` con
  `sdk/rust/user.ld` (0x400000, segmentos R-X, R--, RW-) y los empaqueta con
  `init` en `aricodeos/initrd.img`. `process::spawn` los carga con
  `aricode-elf` y mapea cada segmento con sus permisos. `_start` recibe en
  `rdi` el argumento de quien creó el proceso. Se lanzan con
  `test=user_<nombre>`.

## Objetos y handles (Fase 12, ADR 0010)

- `object::KernelObject` (`Thread`, `Process`, `Console`) se referencia con
  `Arc` desde las tablas de handles; los procesos nunca ven punteros ni
  identificadores globales con valor de acceso.
- Cada proceso nace con tres handles: sí mismo (`MANAGE|READ|DUPLICATE`), su
  hilo (`READ|WAIT|DUPLICATE`) y la consola (`WRITE|DUPLICATE`); límite de
  256 handles por proceso.
- `user.handles` comprueba desde ring 3: tipo de objeto, handle inventado
  (`BadHandle`), duplicado sin derechos que no puede escribir (`Denied`),
  escalada al duplicar (`Escalation`), handle cerrado (`BadHandle`) y uso de
  un objeto del tipo equivocado.

## Capabilities (Fase 13)

- Revocación por objeto: `KernelObject` lleva un flag `revoked`; `object_revoke`
  exige `MANAGE` y a partir de ahí todo handle a ese objeto, en cualquier
  proceso, devuelve `Revoked` (salvo cerrarlo). No hace falta localizar los
  handles: comparten el `Arc`.
- Derivación: `handle_duplicate` solo reduce derechos (Fase 12); un
  duplicado revocado no puede volver a duplicarse.
- Transferencia: `HandleTable::take` retira un handle con `TRANSFER` para
  moverlo a otra tabla conservando sus derechos; es la primitiva que usa el
  IPC (Fase 14) para pasar capabilities entre procesos.
- `capabilities.revoke` (programa Rust `caps`): derivar la consola, intentar
  revocarla sin `MANAGE` (`Denied`), revocar el propio proceso e ver el
  duplicado invalidado, cerrar el revocado y comprobar que la consola sigue
  viva.

## IPC (Fase 14)

- `ipc::Endpoint` es un objeto del kernel: cola acotada (16 mensajes) de
  `Message` (hasta 256 bytes y 4 capabilities), dos colas de espera (emisores
  y receptores) y contador de envíos. `endpoint_create` da al creador
  `READ|WRITE|DUPLICATE|TRANSFER|MANAGE`; el modelo cliente/servidor
  habitual es: el cliente crea un endpoint de respuesta y envía en cada
  mensaje un duplicado con `WRITE|TRANSFER`.
- `ipc_send` exige `WRITE` sobre el endpoint y `TRANSFER` sobre cada handle;
  los handles se retiran del emisor solo cuando la cola confirma la plaza
  (`Endpoint::send_with` construye el mensaje dentro de la sección crítica),
  así que un timeout no pierde capabilities. `ipc_receive` exige `READ`,
  copia los datos y los handles recibidos al espacio del proceso
  (`copy_to_user` valida USER y WRITABLE) y devuelve `len | (n << 32)`.
- Timeouts en ms: `0` no bloquea, `TIMEOUT_INFINITE` espera siempre. Las
  esperas usan `WaitQueue::wait_until_deadline`: el hilo duerme con plazo y
  un `wake` anticipado (`RoundRobin::wake` saca a un dormido de su lista) lo
  despierta antes.
- Tamaños y colas acotadas son la primera defensa frente al agotamiento de
  recursos (sección 52); los mensajes que no caben producen `TooBig`.
- `test=ipc` lanza `ipc_server` e `ipc_client` (programas Rust) con el mismo
  endpoint del kernel (servidor con `READ|WRITE`, cliente solo `WRITE`):
  1000 round trips con handle de respuesta transferido, más recepción sin
  `READ` (`Denied`), timeout, cola llena, envío fallido sin perder el handle,
  mensaje demasiado grande y handle sin `TRANSFER`. Mide el round trip con el
  TSC: ~260 000 ciclos bajo QEMU TCG incluyendo la creación del cliente. El
  número solo sirve como referencia relativa: TCG no es hardware.

## Initrd e init (Fase 15)

- AriBoot carga `\aricodeos\initrd.img` en páginas de tipo Kernel y lo
  entrega en `BootInfo::initrd_*`. Formato propio (`aricode-initrd`): magia,
  número de entradas y, por fichero, nombre y datos alineados a 8 bytes; el
  lector valida la imagen entera al abrirla y no copia nada.
- `cargo xtask image` construye el initrd con `init` y todos los binarios de
  `user/testprogs`; el kernel ya no embebe programas y se compila sin
  variables de entorno.
- Sin `test=` en la cmdline, `kernel_main` lanza `init` con un único handle
  extra: el objeto `SPAWNER` con `MANAGE|DUPLICATE` (handle
  `0x0001_0004`). `process_spawn` exige `MANAGE` sobre un `SPAWNER`, busca el
  nombre en el initrd y devuelve un handle de proceso con
  `READ|WAIT|MANAGE|DUPLICATE`; `process_wait` exige `WAIT`, bloquea en la
  cola de salida del proceso (con plazo opcional) y devuelve el código o
  `Killed`. Un proceso creado así solo tiene los tres handles iniciales.
- `init.boot` (smoke): AriInit lanza `hello` (42), ve `kernel_read` morir
  como `Killed`, comprueba que un duplicado del SPAWNER sin `MANAGE` no
  puede crear procesos y que un nombre inexistente da `NotFound`.

## Drivers en espacio de usuario (Fase 16)

- `acpi::init` localiza el RSDP de `BootInfo`, valida XSDT/RSDT y parsea
  MADT y MCFG con `aricode-acpi` a través del mapa físico. `ioapic::init`
  mapea el primer I/O APIC sin caché en `0xFFFF_D000_0000_1000` y enmascara
  sus 24 entradas.
- Un driver solo recibe lo que sus capabilities le dan (sección 27):
  - `Irq`: GSI enrutada a un vector de dispositivo (`0x30..=0x3F`) con la
    polaridad y disparo de la MADT. El handler acumula y despierta; las
    líneas por nivel quedan enmascaradas hasta que `irq_wait` las atiende.
    Al soltarse el último handle, la línea se enmascara y el vector se libera.
  - `Mmio`: región física de dispositivo; `mmio_map` la mapea en el proceso
    (`0x1000_0000` en adelante, sin caché) y la registra como memoria ajena,
    que se desmapea sin liberar al destruir el espacio.
  - `Dma`: frames contiguos puestos a cero (`dma_alloc`, máximo 1 MiB, exige
    `MANAGE` sobre un objeto `Hardware`); `dma_phys` da la física para el
    dispositivo y `mmio_map` la mapea con caché. Los frames vuelven al PMM al
    soltarse el objeto.
- `test=drivers`: `pit_driver` atiende cinco IRQ0 del PIT (canal 0 a 100 Hz)
  y reserva, mapea y verifica un buffer DMA; `fb_driver` recibe el
  framebuffer como región MMIO y pinta 64x64 píxeles; el kernel comprueba el
  píxel (0,0). El reinicio de un driver caído lo hace AriInit en `init.boot`
  (`crashy_driver` muere con arg 0 y arranca con arg 1).
- Un proceso se inserta en la tabla antes de encolar su hilo
  (`prepare_for_process` + `start_thread`): con `log=trace` la salida lenta
  hacía que el hilo llegara a ejecutar antes de existir su proceso.

## PCIe (Fase 17)

- `pci::init` toma la primera entrada de la MCFG, mapea su ECAM sin caché en
  `0xFFFF_D100_0000_0000` con páginas de 2 MiB (1 MiB por bus) y recorre los
  buses desde el inicial siguiendo los puentes PCI-PCI. Cada función se lee
  con accesos de 32 bits y se decodifica con `aricode-pci`: vendor y device,
  clase, BARs (E/S, 32 y 64 bits, prefetchable) y lista de capabilities
  (MSI, MSI-X, PCIe, PM).
- En QEMU q35: host bridge, VGA, Ethernet e1000e (MSI/MSI-X), ISA bridge,
  SATA AHCI (MSI) y SMBus. `pci.enumerate` (smoke) los exige.
- La lista se devuelve al arranque; la programación de MSI/MSI-X y la
  asignación de BARs a drivers llegan con virtio (Fase 18).

## Virtio-blk en espacio de usuario (Fase 18)

- La raíz PCI es un objeto que solo recibe `init` (`0x0001_0005`,
  `READ|MANAGE`). `pci_count`/`pci_info` enumeran desde usuario; `pci_claim`
  reclama una función: el kernel habilita memoria y bus master, deshabilita
  INTx, sondea el tamaño de cada BAR de memoria, programa la entrada 0 de
  MSI-X hacia un vector de dispositivo (tabla mapeada en una ventana sin
  caché del kernel) y devuelve nueve handles: espacio de configuración
  (MMIO), `Hardware`, IRQ y los seis BARs (0 si ausentes), todos con
  `TRANSFER`.
- `process_spawn` transfiere handles al hijo (aparecen desde
  `0x0001_0004` en orden). `init` reclama `1af4:1042`, compacta los handles
  presentes y pasa en `arg` una máscara (bit 15 IRQ, bits 0..5 BARs); el
  driver reconstruye la posición de cada BAR con ella.
- `services/virtio-blk`: lee el espacio de configuración por MMIO, localiza
  las estructuras common/notify/device con `aricode-virtio`, negocia
  `VERSION_1`, crea la cola 0 en un buffer DMA (`QueueLayout` +
  `Virtqueue`), asigna el vector MSI-X 0 a la cola y ejecuta peticiones de
  un sector (cabecera, datos, estado) esperando la IRQ con `irq_wait`. La
  autoprueba lee sectores del disco de pruebas, escribe uno y lo relee.
- El runner añade `virtio-blk-pci` moderno con `build/disk.img` (4 MiB, GPT
  con la partición `ARI0` formateada como AriFS; desde el LBA 4096 cada
  sector lleva `ARIDISK\0` y su número), regenerado en cada arranque.
- Los programas de usuario tienen un heap estático de 64 KiB
  (`aricode-heap` tras un spinlock) porque la librería PCI usa `alloc`.

## Servicio de bloques (Fase 19)

- `init` crea el endpoint del servicio y se lo pasa al driver junto a las
  capabilities del dispositivo; a los clientes les da un duplicado solo
  `WRITE` y una capacidad de hardware para su buffer DMA (init la recibe del
  kernel en `0x0001_0006`).
- Protocolo `BlockServiceProtocol v1` (`sdk/proto::block`): petición de 16
  bytes (`Info`, `Read`, `Write`, LBA, sectores) con dos handles, el endpoint
  de respuesta y el buffer DMA del cliente; respuesta de 16 bytes con estado
  y valor. El driver programa el descriptor de datos con la física del
  buffer del cliente: cero copias. Hasta 16 sectores por petición; la cola
  del endpoint es la cola de peticiones.
- Al arrancar, el driver lee la GPT (`aricode-gpt`, CRC de cabecera y tabla)
  y anuncia las particiones; el disco de pruebas de xtask lleva un MBR
  protector, GPT con respaldo y una partición `ARI0` de tipo AriCodeOS.
- `blk_client` (ring 3) prueba info, lectura de la partición, escritura y
  relectura, LBA fuera de rango (`Invalid`) y petición sin buffer
  (`BadHandles`). `storage.block_service` lo exige.
- Los programas de usuario tienen la pila en 64 KiB y el kernel diagnostica
  `USER STACK OVERFLOW` cuando un fallo toca su guard page (la tabla GPT de
  16 KiB en la pila lo provocó; ahora va al heap del programa).
- `WaitQueue::wait_until_deadline` mantiene las interrupciones deshabilitadas
  desde la comprobación hasta el cambio de contexto: un `wake` desde un
  handler MSI-X no puede llegar en la ventana entre soltar el lock y
  dormirse (antes cada petición podía esperar hasta el plazo de 2 s).

## Sistema de archivos (Fase 20)

- AriFS v0 (`libs/arifs`): LBA 0 superbloque (magia `ARIFS`, sectores,
  siguiente LBA libre), LBA 1..4 tabla de 32 entradas de 64 bytes (nombre,
  LBA inicial, capacidad, tamaño), datos desde el LBA 5 en bloques fijos
  de 16 sectores asignados de forma incremental. Sin fragmentación, borrado
  ni journaling: lo justo para el criterio de la fase. Todo el acceso pasa
  por el trait `BlockDevice`, así que la librería se prueba en host sobre
  un `Vec` y xtask la usa para formatear la partición `ARI0` del disco de
  pruebas con `hola.txt` y `README`.
- `fsd` (`services/arifs`) recibe de init su endpoint (`READ|WRITE`), el
  del servicio de bloques (`WRITE`) y una capacidad de hardware; `arg` es el
  LBA de la partición. Implementa `BlockDevice` sobre `aricode-sys::block`
  (un sector por petición) y atiende `FileServiceProtocol v1`
  (`sdk/proto::fs`): peticiones y respuestas de 64 bytes con `List`,
  `Open`, `Create`, `Read`, `Write` y `Close`; los datos van por el buffer
  DMA del cliente (hasta 4 KiB por petición), que fsd mapea una vez y
  reutiliza por dirección física. Los descriptores son globales al servicio
  (16 abiertos) hasta que haya identidad de cliente por capability.
- `aricode-sys::fs::FsClient` encapsula endpoint de respuesta, buffer DMA
  y llamadas; `fs_client` (ring 3) lista, lee `hola.txt`, crea y escribe
  `nuevo.txt`, relee y comprueba `Exists`, `NotFound` y `BadFd`.
  `filesystem.read_write` lo exige; `init.boot` exige el mensaje de éxito.
- El área en bruto del disco de pruebas (LBA 4096 en adelante, patrón
  `ARIDISK\0`+LBA) queda fuera del alcance máximo de AriFS (32 × 16
  sectores) para que `blk_client` y `fsd` no se pisen.

## Shell y entrada de consola (Fase 21)

- `console_input` (kernel): la IRQ ISA 4 de COM1 se enruta por el I/O APIC
  al vector `0x40`, fuera del rango de dispositivos de usuario; el handler
  vacía el FIFO en un anillo de 1 KiB y despierta una `WaitQueue`.
  `console_read` (syscall 24) exige `READ` sobre un objeto `Console`,
  valida el destino antes de bloquear y devuelve `Timeout` si vence el plazo.
  Los bytes descartados por anillo lleno se cuentan para diagnóstico.
- Objeto `System` (kind 11): `system_info` (syscall 25, `READ`) responde
  `UPTIME` (ms), `MEMORY` (frames utilizables y libres: el PMM distingue la
  RAM entregada con `free_range` de los frames direccionables) y
  `PROCESSES` (entradas de 32 bytes con pid, estado, código y nombre);
  `system_power` (syscall 26, `MANAGE`) apaga (`ARICODEOS_SHUTDOWN` y
  salida de QEMU con `qemu_exit`, `hlt` sin él) o reinicia (reset por el
  8042 y triple fallo como respaldo). ABI v3.
- Init recibe además `SYSTEM` (`0x10007`) y una entrada de consola
  (`0x10008`, `READ`); el kernel le pasa en `arg` `init_arg::SHELL` si la
  cmdline lleva `shell`. Tras los servicios de arranque lanza `shell` con
  el endpoint de fsd (`WRITE`), hardware, `SYSTEM`, la entrada de consola
  y la raíz PCI solo lectura, y `arg` = máscara de servicios activos; espera
  su salida y la anota.
- `arish` lee líneas con eco y retroceso, y despacha los comandos sin
  `alloc` (formato con `fmt_u64`/`fmt_hex` de la biblioteca de programas
  de usuario). `cat` lee por bloques de 1 KiB con `FsClient`.
- El runner (`qemu::run_headless`) acepta `Input { prompt, lines }`: lee la
  serie de forma incremental y envía la línea `k` cuando el prompt ha
  aparecido `k + 1` veces, así los tests son deterministas sin esperas
  fijas. `console.input` usa `CONSOLE_INPUT_READY` como prompt para probar
  el kernel sin shell.

## Deuda conocida

- `arish` no tiene historial, edición de línea ni redirecciones; las
  variables de entorno y el lanzamiento de programas llegan con el servicio
  de procesos.
- `fsd` no distingue clientes: cualquier proceso con su endpoint ve todos
  los descriptores. La capability por fichero llega con la identidad de
  cliente en IPC.

- El servicio de bloques mapea cada buffer DMA de cliente una vez y lo
  reutiliza por dirección física; no hay `mmio_unmap` ni caché de bloques.
- El device manager (sección 29) vive dentro de `init`: enumera, reclama y
  lanza drivers; separarlo en un servicio propio cuando haya más de uno.
- Sin IOMMU (Fase 33): un buffer DMA es memoria autorizada solo por
  convención del driver.
- `init` todavía no recibe endpoints de los servicios que lanza ni puede
  pasarles capabilities al crearlos: llegará con el device manager (Fase 16).
- Los benchmarks (sección 69) necesitan una máquina de referencia o KVM
  para ser absolutos; bajo TCG solo comparan versiones entre sí.
- Los mensajes IPC no llevan identidad del emisor: la dará un handle al
  proceso remitente cuando haya política de identidad (sección 80).
- Sin SMAP/SMEP: el kernel puede tocar memoria de usuario sin querer; los
  accesos se limitan a `copy_from_user` validado. Activar SMEP/SMAP cuando la
  CPU de referencia lo garantice.
- Un proceso tiene un único hilo y su espacio es fijo (sin `mmap`): las
  syscalls `memory_map`/`memory_unmap` y la creación de hilos desde usuario
  son parte del conjunto previsto en la sección 22 y llegarán con el IPC
  (Fase 14) y el runtime (Fase 27).
- Sin prioridades ni afinidad: solo round robin en una CPU (Fase 32 para SMP).
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
- La invalidación de TLB es local y explícita en cada ruta que desmapea
  (heap al deshacer un crecimiento, pilas de hilo al liberarse, autoprueba
  del VMM, con `invlpg`); no existe un mecanismo genérico ni TLB shootdown
  entre CPUs, que llegará con SMP (Fase 32).
- El loader depende de la crate `uefi` (ver ADR 0006).
