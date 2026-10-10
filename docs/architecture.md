# Arquitectura

La referencia completa es el documento maestro [../AriCodeOS.md](../AriCodeOS.md).
Este fichero resume lo implementado hasta ahora y dónde vive cada pieza.

## Estado: 0.8.1 (Fases 0 a 34 y etapas 1 a 5 posteriores al roadmap; la 34, la energía y más dispositivos pendientes de hardware)

```text
OVMF (UEFI)
  -> EFI/BOOT/BOOTX64.EFI            AriBoot        boot/
       GOP, ACPI RSDP, cmdline.txt, kernel.elf, initrd.img, tablas de
       páginas (identidad + mapa físico lineal + kernel), BootInfo v4,
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
       de bloques por IPC (disco GPT), fsd monta AriFS v1 sobre él y sirve
       ficheros por IPC; con `shell` en la cmdline init lanza arish, que lee
       la consola serie y apaga o reinicia por `system_power`; netd
       (virtio-net) obtiene IP por DHCP y sirve ping, UDP y DNS por IPC;
       SMP: los APs de la MADT arrancan por INIT-SIPI y planifican sobre la
       misma cola de listos con estado por CPU e IPI; IOMMU VT-d con un
       dominio por driver (IOVA = física); ARICODEOS_KERNEL_READY
```

## Crates

```text
boot-protocol/       ABI BootInfo (v4): mapa UEFI, framebuffer, RSDP, kernel, pila, PML4 y tablas, BootInfo, cmdline, mapa físico, rangos conservados
libs/elf/            aricode-elf: parser ELF64 mínimo (no_std, tests de host)
libs/x86-paging/     aricode-x86-paging: tablas de 4 niveles genéricas en FrameAlloc/PhysMap (no_std, tests de host)
libs/cmdline/        aricode-cmdline: `clave` / `clave=valor` sin alloc (no_std, tests de host)
libs/fb-console/     aricode-fb-console: consola de texto sobre framebuffer 32 bpp, fuente Terminus 8x16 (OFL), tests de host
libs/x86-cpu/        aricode-x86-cpu: descriptores GDT/TSS/IDT y tabla de excepciones (no_std, tests de host)
libs/pmm/            aricode-pmm: allocator de frames por bitmap sobre almacenamiento externo (no_std, tests de host)
libs/heap/           aricode-heap: heap first-fit con lista de libres, fusión y cabecera por asignación (no_std, tests de host)
libs/sync/           aricode-sync: IrqLock/SpinLock con política de interrupciones enchufable, restaurada entre giros; force_unlock (no_std, tests de host)
libs/fat/            aricode-fat: FAT32 mínimo con nombres largos para construir y leer la ESP de la imagen USB (no_std + alloc, tests de host)
libs/xhci/           aricode-xhci: xHCI y USB: registros, TRB, anillos, contextos, ERST, SETUP y descriptores (no_std, tests de host)
libs/usb-storage/    aricode-usb-storage: transporte bulk-only (CBW/CSW) y CDB SCSI de bloque, tests de host
libs/rtl8169/        aricode-rtl8169: registros, descriptores y arranque de la familia r8169 sobre un trait Regs, tests de host
libs/iommu/          aricode-iommu: Intel VT-d: registros, capacidades, entradas raíz/contexto, paginación de segundo nivel, registros de fallo (no_std, tests de host)
libs/sched/          aricode-sched: política round robin sobre Tid con estados, sueño, bloqueo, salida, varias CPUs, afinidad y despertares pendientes (no_std + alloc, tests de host)
libs/handles/        aricode-handles: tabla de handles con generación y derechos, duplicación solo reductora (no_std + alloc, tests de host)
libs/initrd/         aricode-initrd: formato del initrd, lector sin copia (no_std) y escritor (feature `write`, xtask); tests de host
libs/acpi/           aricode-acpi: RSDP, XSDT/RSDT, MADT (LAPIC, I/O APIC, overrides), MCFG; sin AML (no_std + alloc, tests de host)
libs/pci/            aricode-pci: direccionamiento ECAM, cabecera de configuración, BARs, capabilities, clases (no_std + alloc, tests de host)
libs/virtio/         aricode-virtio: virtqueue split sobre DMA, estructuras virtio-pci modernas, peticiones virtio-blk (no_std, tests de host)
libs/gpt/            aricode-gpt: CRC32, cabecera y entradas GPT, escritor para el disco de pruebas (no_std, tests de host)
libs/ab/             aricode-ab: estado de arranque A/B, imágenes de sistema firmadas y política de slots (no_std, tests de host)
libs/crypto/         aricode-crypto: SHA-256, SHA-512, Ed25519 y ChaCha20 desde cero con vectores oficiales (no_std, tests de host)
libs/wasm/           aricode-wasm: intérprete WebAssembly MVP con enteros, memoria acotada e importaciones (no_std + alloc, tests de host)
libs/manifest/       aricode-manifest: manifiestos de aplicación (nombre, capacidades, memoria) (no_std, tests de host)
libs/gfx/            aricode-gfx: lienzos 0x00RRGGBB, recorte, texto, cursor y escritorio de ventanas (no_std, tests de host)
libs/input/          aricode-input: eventos evdev y teclado US (no_std, tests de host)
libs/http/           aricode-http: GET, análisis de respuestas, Content-Length y chunked sin alloc (no_std, tests de host)
libs/net/            aricode-net: Ethernet, ARP, IPv4, ICMP, UDP, TCP, cliente DHCP y DNS sin alloc (no_std, tests de host)
libs/arifs/          aricode-arifs: AriFS v1, directorio plano con tabla de ficheros de capacidad fija, CRC32 y journal de reemplazo sobre `BlockDevice` (no_std, tests de host con cortes)
libs/pkg/            aricode-pkg: paquetes de aplicación firmados (manifiesto con nombre, versión y hashes; firma Ed25519; ELF; manifiesto de aplicación)
sdk/proto/           aricode-proto: protocolos IPC versionados de los servicios (bloques v1, ficheros v1, red v1, entrada v1, ventanas v1, lanzamiento v1) (no_std, tests de host)
sdk/runtime/         aricode-rt: runtime de programas de usuario (`entry!`, heap de 256 KiB, panic, fmt); reexporta aricode-sys
sdk/build/           aricode-build: `build.rs` de una línea que enlaza con `sdk/rust/user.ld`
apps/updater/        updater: verifica la imagen de UPDATE y la instala en el slot inactivo (capacidad `block`)
apps/wasm-demo/      wasm_demo.wasm: módulo portable compilado a wasm32 que solo usa la Capability API importada
apps/wasm-runner/    wasm_runner: runtime WebAssembly como aplicación con manifiesto (lee el módulo de su directorio privado)
apps/hello/          hello_app: aplicación de ejemplo del SDK (consola, permisos, tiempo)
sdk/abi/             aricode-abi: números de syscall, errores, handles iniciales, derechos y tipos de objeto (no_std, tests de host)
sdk/rust/            aricode-sys: wrappers de syscall (no_std, sin alloc), clientes de los servicios de bloques, ficheros y red, cliente HTTP GET, transporte virtio PCI para drivers, y `user.ld`, el linker script de todo programa de usuario
user/testprogs/      programas de prueba de ring 3 (Rust y ensamblador) enlazados como ELF estáticos en 0x400000
services/init/       AriInit: primer proceso; recibe SPAWNER y la raíz PCI, lanza/supervisa servicios y reclama dispositivos para sus drivers
services/virtio-blk/ driver virtio-blk en ring 3 y servicio de bloques por IPC (interfaz moderna, cola 0 en DMA, MSI-X, GPT)
services/virtio-net/ netd: driver virtio-net y pila de red (DHCP, ARP, ICMP, UDP, TCP, DNS) por IPC en ring 3
services/net-core/   ari-net-core: pila IPv4 mínima y servidor NetServiceProtocol, independientes de la tarjeta (trait Link, ADR 0024)
services/rtl-net/    driver Realtek r8169 (RTL8169/8168/8101) en ring 3 sobre ari-net-core; sin emulación en QEMU
services/compositor/ compositor: dueño del framebuffer, ventanas, cursor y enrutado de entrada en ring 3
services/virtio-input/ inputd: driver virtio-input (un proceso por teclado o ratón) y eventos por IPC en ring 3
services/arifs/      fsd: servicio de ficheros AriFS en ring 3 sobre el servicio de bloques
services/shell/      arish: shell interactivo en ring 3 (help, ps, mem, ls, cat, write, devices, services, uptime, cpus, diag, net, run, spawn, repeat, reboot, shutdown, exit; historial con las flechas)
boot/                ariboot: integra lo anterior con la crate `uefi`
kernel/              arikernel: serial, framebuffer (Surface MMIO), console (log::Log con IrqLock y try_lock),
                     arch/x86_64 {gdt, idt, interrupts (stubs.s), exceptions, port, pic, pit, apic, ioapic, msr, syscall, usermode},
                     acpi (localización de tablas), pci (ECAM, enumeración, reclamación con sondeo de BARs y MSI-X),
                     hw (IrqLine por GSI, MSI-X o MSI, Region, DmaBuffer, ventanas sin caché, registro de vectores, banco de pruebas),
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

Definido en `boot-protocol/` (`BootInfo`, versión 4, ADR 0009 y 0021). Al entrar al kernel:

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

Toda la memoria que AriBoot reserva es `LOADER_DATA` (ADR 0021: con tipos
propios algunos firmwares se cuelgan en `GetMemoryMap`). En el mapa es
memoria de arranque (`Boot`); `BootInfo::kernel_ranges()` (imagen, pila
inicial, página del `BootInfo`, initrd, cmdline) dice qué conserva el kernel
y `page_tables_base/size` qué tablas recupera al activar las suyas.

## Consola y logging (Fase 3)

- `print!`/`println!` escriben texto sin formato en serie y framebuffer
  (banner y marcadores como `ARICODEOS_KERNEL_READY`).
- Las macros de la crate `log` (`trace!`..`error!`) pasan por `console::KernelLogger`,
  que antepone `[LEVEL]` y escribe en ambas salidas. Nivel por defecto `info`;
  `log=<nivel>` en la cmdline lo cambia y un valor inválido produce un `WARN`.
- El panic handler escribe solo por serie (`serial_println!`): no toca estado
  compartido, por si el panic ocurre a medio escribir en el framebuffer.
- La consola de framebuffer mantiene una copia de las celdas y repinta al
  hacer scroll; nunca lee de VRAM. Entiende el retroceso (`\x08`, el cursor
  vuelve una celda) y pinta un cursor `_` que alterna un hilo del kernel
  (`console::start_cursor_thread`, cada 500 ms, con `try_lock`). Ni el tick
  (no se pinta desde handlers) ni el bucle idle: una llamada más en ese
  bucle, que gira mucho, frenaba todo el sistema en QEMU un 25 %. Solo
  parpadea con la consola quieta (sin escrituras desde el parpadeo
  anterior); cualquier escritura lo oculta antes de pintar, así que las
  celdas nunca contienen el cursor.
- Las teclas de edición (flechas, inicio, fin, suprimir) llegan a la consola
  como las secuencias de escape de un terminal serie (`ESC [ A`…, `ESC [ 3 ~`),
  vengan del teclado PS/2 del kernel, del puente de consola o del compositor:
  `aricode_input::Keyboard::feed` devuelve `KeyBytes` (hasta 4 bytes).

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

Todo el estado global (consola de framebuffer, PMM, VMM, heap, IDT,
planificador, tabla de procesos, colas de espera) vive tras `sync::IrqLock`,
que guarda `RFLAGS.IF`, ejecuta `cli` al tomar y restaura al soltar: una
sección crítica no puede ser interrumpida ni reentrada por un handler. Entre
giros de espera las interrupciones vuelven a su estado (Fase 32): una CPU que
espera un lock de otra sigue atendiendo IPI. Un lock tomado durante muchos
giros es reentrada (una CPU) o bloqueo mutuo, así que `lock` entra en pánico
en vez de colgar. La consola usa `try_lock` y cae a solo serie si está tomada
(una excepción dentro de la propia consola no bloquea el diagnóstico). El
estado propio de cada CPU (GDT, TSS, hilo actual, quantum) no lleva lock:
solo lo toca su CPU con interrupciones deshabilitadas (`cpu::Cpu`).

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
planificador antes de liberar un espacio de usuario). El lock del
planificador es la excepción al patrón guard: se mantiene tomado durante el
cambio de contexto y lo suelta el hilo que reanuda con `force_unlock` (o
`thread_first_run` en su primera ejecución), porque con varias CPUs el hilo
saliente no puede quedar en la cola de listos antes de que su `rsp` esté
guardado. `smp::flush_others` (TLB shootdown) exige interrupciones
habilitadas y ningún lock tomado: espera a las demás CPUs y estas pueden
estar esperando el mismo lock. El lock de
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
- `syscall_entry` hace `swapgs`, guarda el `rsp` de usuario en el scratch de
  la CPU (`gs:`), carga la pila de kernel del hilo, construye un
  `SyscallFrame`, habilita interrupciones y despacha en Rust; antes de
  `sysretq` las deshabilita y deshace el `swapgs`. Convención en `sdk/abi`: `rax`
  número, `rdi rsi rdx r10 r8 r9` argumentos, resultado en `rax`, negativo
  `-4095..=-1` = error. Syscalls actuales: `process_exit`, `console_write`,
  `thread_yield`, `thread_sleep`, `thread_get_tid`, `abi_version`,
  `handle_close`, `handle_duplicate`, `object_kind`, `object_id`,
  `object_revoke`, `endpoint_create`, `ipc_send`, `ipc_receive`,
  `process_spawn` (con transferencia de handles al hijo), `process_wait`,
  `irq_wait`, `mmio_map`, `region_len`, `dma_alloc`, `dma_phys`, `pci_count`,
  `pci_info`, `pci_claim`, `console_read`, `system_info`, `system_power`,
  `initrd_read`, `pci_info_ext`, `console_inject`, `process_spawn_from`,
  `ipc_receive_any`, `process_kill` (`MANAGE` sobre el proceso: marca la
  terminación y despierta a su hilo, que termina al salir de la syscall en
  curso, al despertar de una espera o en el siguiente tick si estaba en
  ring 3; `process_wait` devuelve `Killed`; auditoría 6ee19fb, 4).
  Los servicios de bloques, ficheros y red no añaden syscalls: son IPC.
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
- `ipc_receive_any` (syscall 31, ADR 0020) recibe del primero de hasta 16
  endpoints con mensaje (todos con `READ`, validados antes de esperar) y
  devuelve su índice en los bits 48..63; los receptores múltiples esperan
  en una cola común que despierta cualquier envío. fsd espera así en su
  endpoint principal y en los concedidos a la vez, sin sondeos de 10 ms.
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

- AriBoot carga `\aricodeos\initrd.img` en páginas que el kernel conserva y lo
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
  caché del kernel) o, si la función no tiene MSI-X, su capability MSI con
  un solo mensaje (ADR 0022), y devuelve nueve handles: espacio de configuración
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
- Los programas de usuario tienen un heap estático de 256 KiB
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

- AriFS v1 (`libs/arifs`, ADR 0014): LBA 0 superbloque (magia `ARIFS`,
  sectores, siguiente LBA libre, CRC32), LBA 1..4 tabla de 32 entradas de
  64 bytes (nombre, LBA inicial, capacidad, tamaño, CRC32 por entrada), LBA
  5 cabecera del journal de reemplazo y 6..69 sus datos, datos desde el LBA
  70 en bloques fijos de 64 sectores asignados de forma incremental. Sin
  fragmentación ni borrado. `open` rechaza (`Corrupt`) metadatos cuyo CRC
  no cuadra y rehace un reemplazo comprometido a medias; `write` escribe en
  el sitio (sector entero viejo o nuevo tras un corte, tamaño después de
  los datos, duradero tras `flush`); `replace` es atómico y duradero por el
  journal (datos + CRC, `flush`, cabecera pendiente, `flush`, copia y
  tamaño, `flush`, limpieza). Todo el acceso pasa por el trait
  `BlockDevice` (con `flush`), así que la librería se prueba en host sobre
  un `Vec`, incluido un corte en cada escritura del reemplazo con la caché
  perdida o conservada, y xtask la usa para formatear la partición `ARI0`
  del disco de pruebas con `hola.txt` y `README` (y fabricar los discos
  `fs-pending-replace` y `fs-corrupt`).
- `virtio-blk` negocia `VIRTIO_BLK_F_FLUSH` y atiende `Op::Flush` del
  protocolo de bloques (`T_FLUSH`); sin la característica, `Flush` vuelve
  sin hacer nada y el driver lo anuncia al arrancar.
- `fsd` (`services/arifs`) recibe de init su endpoint (`READ|WRITE`), el
  del servicio de bloques (`WRITE`), una capacidad de hardware y `SYSTEM`
  (lectura, reloj para caducar reemplazos por trozos abandonados); `arg` es
  el LBA de la partición. Implementa `BlockDevice` sobre `aricode-sys::block`
  (lotes de hasta 8 sectores por petición: la librería agrupa los sectores
  enteros de cada lectura, escritura, trozo del journal y aplicación, etapa
  5) y atiende `FileServiceProtocol v1`
  (`sdk/proto::fs`): peticiones y respuestas de 64 bytes con `List`,
  `Open`, `Create`, `Read`, `Write`, `Close`, `Flush`, `Replace`
  (reemplazo atómico de hasta `MAX_IO`) y `ReplaceChunk`/`ReplaceCommit`
  (trozos al journal y compromiso: hasta 32 KiB con la misma garantía; un
  solo reemplazo a medias por servicio, `Busy` para los demás, liberado por
  el compromiso, un error o el cierre del descriptor; ADR 0018); los datos van por el buffer
  DMA del cliente (hasta 4 KiB por petición), que fsd mapea una vez y
  reutiliza por dirección física. Los descriptores son globales al servicio
  (16 abiertos) hasta que haya identidad de cliente por capability.
- `aricode-sys::fs::FsClient` encapsula endpoint de respuesta, buffer DMA
  y llamadas; `fs_client` (ring 3) lista, lee `hola.txt`, crea y escribe
  `nuevo.txt`, lo reemplaza de una vez, hace `flush`, relee y comprueba
  `Exists`, `NotFound` y `BadFd`. `filesystem.read_write` lo exige (y el
  `flush` negociado); `init.boot` exige el mensaje de éxito;
  `filesystem.replace_recovery` rehace al montar un reemplazo cortado tras
  el compromiso y `filesystem.corrupt_detected` rechaza una tabla alterada.
- El área en bruto del disco de pruebas (LBA 4096 en adelante, patrón
  `ARIDISK\0`+LBA) queda fuera del alcance máximo de AriFS (70 + 32 × 64
  sectores) para que `blk_client` y `fsd` no se pisen.

## Shell y entrada de consola (Fase 21)

- `console_input` (kernel): la IRQ ISA 4 de COM1 se enruta por el I/O APIC
  al vector `0x40`, fuera del rango de dispositivos de usuario; el handler
  vacía el FIFO en un anillo de 1 KiB y despierta una `WaitQueue`.
  `console_read` (syscall 24) exige `READ` sobre un objeto `Console`,
  valida el destino antes de bloquear y devuelve `Timeout` si vence el plazo.
  Los bytes descartados por anillo lleno se cuentan para diagnóstico.
- Teclados de usuario (etapa 2, ADR 0013): `console_inject` (syscall 29)
  escribe texto en el mismo anillo y exige `MANAGE` sobre el `Console`;
  solo init lo tiene (`0x10008` con `READ|MANAGE|DUPLICATE|TRANSFER`) y lo
  delega al servicio `console_bridge`, que en modo `shell` sin compositor
  recorre los servicios de entrada (virtio-input, USB HID) por turnos y
  traduce las pulsaciones con `aricode_input::Keyboard`. Es un residente
  supervisado más (`services --detail`); los escenarios `shell.usb_keyboard`
  y `shell.virtio_keyboard` teclean una sesión por QMP.
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
  de usuario). `cat` lee por bloques de 1 KiB con `FsClient`. Las flechas
  arriba y abajo (`ESC [ A` / `ESC [ B`) recorren un historial de las ocho
  últimas líneas distintas, que se pueden editar antes de ejecutar; el resto
  de secuencias de escape se descartan (escenario `shell.history`). `diag
  NOM` captura la salida del informe (consola y buffer de 16 KiB) y la guarda
  en AriFS; `cargo xtask fs-cat DISCO [NOMBRE]` la lee desde el anfitrión
  (escenario `shell.diag_file`, test de `fscat` sobre el disco de pruebas).
  El informe incluye `net`: `Op::Link` (14) del protocolo de red responde el
  estado del enlace físico (`LINK_UP`/`LINK_DOWN`/`LINK_UNKNOWN`) aunque
  DHCP no haya terminado, y el resumen de `services` nombra papeles
  (bloques, red, entrada), no drivers.
- El runner (`qemu::run_headless`) acepta `Input { prompt, lines }`: lee la
  serie de forma incremental y envía la línea `k` cuando el prompt ha
  aparecido `k + 1` veces, así los tests son deterministas sin esperas
  fijas. `console.input` usa `CONSOLE_INPUT_READY` como prompt para probar
  el kernel sin shell.

## Red (Fase 22)

- `aricode-net` es una biblioteca de codificadores puros sobre buffers del
  llamador (sin `alloc`): cada capa tiene `parse`/`write` con validación de
  longitudes y checksums, y las máquinas de estado (caché ARP, cliente
  DHCP) se prueban en host paso a paso. El servicio solo conecta la
  biblioteca con el driver y con IPC.
- `aricode-sys::virtio_pci` concentra el transporte virtio moderno
  (estructuras PCI, negociación `VERSION_1 | deseadas`, colas en DMA con
  el vector MSI-X 0, `DRIVER_OK`, `NEEDS_RESET`) y el contrato de handles
  con init; tras los BARs vienen los extras del driver (`extra_handle`).
  virtio-blk quedó reducido a su lógica de peticiones.
- `netd` es de un solo hilo: en cada vuelta vacía la cola de recepción,
  atiende hasta 8 peticiones IPC con timeout 0, vence plazos y espera la
  IRQ hasta 10 ms. Las operaciones que esperan a la red (ping, `UdpRecv`,
  `Dns`) guardan el endpoint de respuesta del cliente y contestan cuando
  llega el paquete o vence el plazo; las tramas cuyo siguiente salto no
  está en la caché ARP se aparcan hasta la respuesta ARP (1 s).
- La red de QEMU es la de usuario (slirp): DHCP entrega 10.0.2.15,
  router 10.0.2.2 (que responde a ICMP y redirige UDP al loopback del
  host) y DNS 10.0.2.3. xtask arranca con cada QEMU un respondedor UDP en
  `127.0.0.1:17007` que hace eco y contesta consultas DNS con `10.1.2.3`,
  así `network.stack` y el shell no dependen de Internet. Al declarar un
  `netdev`, QEMU deja de añadir la e1000 por defecto: el disco pasa a
  `00:02.0` y la red a `00:03.0` (7 funciones).

## TCP (Fase 23)

- `aricode-net::tcp::Connection` es una máquina de estados pura: `poll`
  produce como mucho un segmento por llamada (retransmisión vencida, SYN,
  datos nuevos dentro de la ventana del otro extremo, FIN, ACK puro) y
  `on_segment` consume uno; el llamador transmite y cronometra. Buffers
  fijos de 4 KiB por sentido, retransmisión desde `snd_una` con RTO
  exponencial (500 ms a 8 s, 6 reintentos → `CloseReason::Timeout`),
  TIME-WAIT de 2 s, RST recibido o `abort` → `CloseReason::Reset`. Los
  tests de host conectan dos `Connection` por un cable que descarta
  segmentos elegidos o al azar.
- En `netd` cada socket TCP (`Box`, 8 KiB de buffers) vive en un slot de
  la tabla; los segmentos se demultiplexan por cuádrupla y un SYN a un
  puerto en escucha crea una conexión pasiva que `TcpAccept` entrega al
  establecerse. `TcpConnect`/`TcpAccept`/`TcpRecv` quedan pendientes con
  plazo; `TcpSend` encola lo que cabe y devuelve la cuenta (el cliente
  reintenta con `tcp_send_all`). Un slot se libera cuando el usuario ha
  cerrado y la conexión terminó.
- QEMU redirige `127.0.0.1:17009` al puerto 7 del invitado (`hostfwd`) y
  el respondedor de xtask escucha eco TCP en `127.0.0.1:17008`. Un
  escenario puede declarar `[host_tcp] marker/payload`: al ver el marcador
  en la serie, xtask se conecta al puerto redirigido, envía la carga y
  añade a la captura una línea `[host] sonda TCP: ...` que `serial_contains`
  puede exigir.
- El heap estático de los programas de usuario pasa a 256 KiB (los
  sockets TCP de netd no caben en la pila de 64 KiB).

## Cliente HTTP (Fase 24)

- `aricode-http` no toca la red: `fetch` hace toda la descarga sobre un
  trait `Transport` (`send_all`, `recv` con `Ok(0)` = cierre limpio), así
  que la crate se prueba en host con un transporte simulado y el SDK
  (`aricode-sys::http::get`) solo resuelve, conecta y adapta `NetClient`.
  `parse_response` recoge todas las cabeceras antes de decidir el framing
  (`Transfer-Encoding: chunked` prevalece sobre `Content-Length`; otras
  codificaciones o longitudes contradictorias son inválidas), las
  respuestas 1xx se consumen antes de la final, `ChunkDecoder` es una
  máquina de estados incremental (trozos mayores que el temporal, límites
  de línea entre lecturas, tamaños validados sin aritmética sin comprobar)
  y un cuerpo delimitado por cierre solo termina con `Ok(0)`: un reset del
  transporte es error, nunca éxito parcial. El cuerpo va a un buffer del
  llamador (4 KiB en el shell). El respondedor de xtask añade un servidor
  HTTP de pruebas en `127.0.0.1:17010` con los casos de `network.http`.
- Los identificadores no se recortan: `fs::Request::with_name` devuelve
  `None` si el nombre no cabe y el nombre DNS viaja entero en el buffer DMA
  del cliente (hasta 253 bytes) en vez de en el mensaje IPC.

## Entrada (Fase 25)

- virtio-input entrega eventos evdev (`type`, `code`, `value`, 8 bytes) por
  su cola 0; `inputd` publica 64 buffers de 8 bytes en DMA, repone cada
  uno al consumirlo y guarda los eventos en un anillo de 128 (los más
  antiguos se descartan si nadie lee). El nombre del dispositivo se lee de
  `virtio_input_config` (select `ID_NAME`). Init lanza un `inputd` por
  cada función `1af4:1052` con su propio endpoint y `SYSTEM` como extra.
- `InputServiceProtocol v1`: `Info` (nombre) y `Read(timeout)` con hasta
  4 eventos por respuesta; una sola lectura pendiente por servicio
  (`Busy` si hay otra). `aricode-input::Keyboard` traduce en el cliente,
  no en el driver: el driver no decide la distribución de teclado.
- QEMU lleva `virtio-keyboard-pci` y `virtio-mouse-pci` (funciones
  `00:04.0` y `00:05.0`; nueve funciones PCI en total) y en las
  ejecuciones headless expone QMP por socket Unix en el directorio del
  escenario. `[host_input]` en un escenario hace que xtask, al ver el
  marcador, envíe `input-send-event` (teclas por nombre qcode, movimiento
  relativo y clic); sin `device`, porque QMP solo admite ahí pantallas y
  los dispositivos virtio, registrados después del i8042, son los
  manejadores activos. El resultado se anota como `[host] QMP: ...`.
- Varios arranques sobre el mismo disco (`[[boots]]`, etapa 3): cada
  arranque lleva su `cmdline`, sus líneas de consola, opcionalmente
  `kill_on` (xtask mata QEMU al ver ese texto: un corte de energía) y su
  `expect` (`killed = true` cuando se espera el corte). El disco de
  pruebas se fabrica una vez y se conserva; cada arranque tiene su ESP y
  su `serial-N.log`. Así se prueban `update.full_cycle` (instalar,
  reiniciar, confirmar, arrancar de B), `update.cut_during_install`,
  `update.rollback_keeps_data` y `shell.write_persists`.

## GUI básica (Fase 26)

- Con `gui` en la cmdline el kernel no inicializa la consola de framebuffer
  (solo serie) y `run_init` entrega a init un objeto `Mmio` con el
  framebuffer GOP (`0x10009`, `READ|WRITE`); `system_info(FRAMEBUFFER)`
  devuelve ancho, alto, stride, formato, tamaño y máscaras. Sin `gui` la
  capability existe pero la pantalla la sigue usando el kernel.
- `aricode-gfx` no reserva memoria: `Canvas` envuelve `&mut [u32]` con
  stride y recorta todo (`fill_rect`, `blit`, `text`, `cursor`); `Desktop`
  guarda hasta 8 ventanas con orden z, foco, puntero y colocación en
  cascada, y `compose` pinta fondo, marcos (título activo/inactivo),
  contenidos (vía callback del llamador) y cursor. Todo probado en host
  sobre `Vec<u32>`, incluida la composición de dos ventanas solapadas.
- El compositor compone en un buffer trasero DMA (la pantalla entera,
  `DMA_MAX_LEN` sube a 8 MiB) y lo vuelca al framebuffer convirtiendo a RGB
  si hace falta. Recompone solo cuando algo cambió (`Present`, puntero,
  foco, ventanas) y anota por serie cada frame con una muestra de píxel por
  ventana, que los escenarios exigen. Los eventos de los `inputd` llegan
  con el instante en que cada servicio los recibió (`Reply::times`), y el
  compositor los fusiona por ese instante antes de procesarlos: un clic
  del ratón precede a la tecla que vino después aunque los dispositivos se
  lean por turnos.
- `WindowProtocol v1`: el cliente reserva el buffer de contenido con su
  capacidad de hardware y lo transfiere en `Create` (el compositor lo mapea
  una vez y lo lee en cada composición); `Present` marca la ventana sucia;
  `Poll` espera con plazo un `Key`, `Click` o `Focus`; `Close` la quita.
  El compositor traduce el teclado (`aricode-input::Keyboard`) y entrega
  solo a la ventana con foco: la sonda de `gui.two_windows` comprueba que
  la otra ventana no ve nada.
- La sonda QMP de xtask envía ahora ratón, clic y teclas en ese orden con
  100 ms entre fases.

## SDK de aplicaciones (Fase 27)

- El runtime de usuario (`_start`, heap estático, panic handler, `fmt_u64`
  y `fmt_hex`) vive en `aricode-rt`; `user/testprogs` vuelve a ser solo
  binarios de prueba y todos los servicios dependen del mismo runtime. Los
  ocho `build.rs` duplicados se redujeron a `aricode_build::user_program()`,
  que apunta al linker script del SDK desde su propio `CARGO_MANIFEST_DIR`.
- `aricode-sys` gana `time` (reloj sobre `SYSTEM`) y `permissions`
  (recorrer los handles recibidos saltando los huecos que deja renunciar a
  uno, comprobar tipos, cerrar los que sobran).
  `docs/sdk.md` describe el contrato completo de un programa.
- El shell recibe un `SPAWNER` (init ahora puede transferir el suyo) y
  ejecuta `run PROGRAMA [ARG]`: el hijo solo recibe la consola, su proceso,
  su hilo y un `SYSTEM` de solo lectura. `hello_app` lo demuestra en
  `shell.commands`.

## Medidas (etapa 5)

- `apps/bench` mide desde ring 3 la latencia de una syscall, de una
  llamada IPC completa a fsd y de `replace`/`read` de 4 KiB con percentiles
  sobre el TSC calibrado contra `uptime_ms`; `docs/performance.md` fija el
  método y guarda la referencia de cada versión. `perf.bench` exige que las
  medidas se produzcan, no sus valores (dependen del host).
- Reloj de microsegundos: `apic::init` mide el TSC contra el PIT junto al
  timer y `time::uptime_us` lo convierte (supone TSC invariante y común a
  las CPUs; sin calibración cae al tick de 10 ms); `sysinfo::UPTIME_US`.
- Sucesos recientes del kernel (`events.rs`): anillo de 64 entradas con
  procesos creados, salidos y matados (con el motivo), fallos de DMA del
  IOMMU y cuotas de DMA agotadas, con su instante en µs; `sysinfo::EVENTS`
  los publica a quien tenga `READ` sobre `SYSTEM` y `diag` los muestra
  (`diag.events`). Sin contenido de mensajes ni de ficheros.
- Reposo por CPU: el bucle idle acumula los ciclos de TSC pasados en `hlt`
  (`Cpu::idle_cycles`) y `sysinfo::IDLE` los publica junto a los ciclos
  desde el primer tick; `cpus` los muestra como porcentaje y `diag` reúne
  `uptime`, `mem`, `cpus`, `ps` y `services --detail` en un informe.

## Sandbox de aplicaciones (Fase 28)

- `process_spawn` recibe en su sexto argumento el número de handles y la
  cuota de memoria DMA del hijo (`spawn_limits::pack`); el proceso la guarda
  (`dma_quota`, `dma_used`) y `dma_alloc` cobra el tamaño redondeado a
  página, devolviendo `Limit` al excederla. La cuota es acumulativa: lo
  reservado no se devuelve al liberar (los buffers DMA viven lo que el
  proceso).
- `initrd_read` (syscall 27) copia un fichero del initrd a quien tenga
  `READ` sobre el `SPAWNER`; init lee `NOMBRE.manifest` y `aricode-manifest`
  lo analiza (`name`, `caps`, `memory` en K/M, límite 8 MiB).
- El broker es init: `Broker::serve_one` atiende `LaunchProtocol v1` en el
  bucle que antes solo esperaba al shell. `launch` deniega manifiestos que
  piden capacidades del sistema, pide a fsd un directorio privado (`Bind`)
  cuando la aplicación quiere `fs`, duplica los endpoints de red,
  compositor y entrada solo si se piden, añade `HARDWARE` solo cuando hay
  buffers que reservar y lanza con la cuota del manifiesto; responde con el
  código de salida o `Killed` cuando el programa termina, sin bloquear
  init: `launch` deja el proceso en una de las `MAX_RUNNING` (4) ranuras y
  `poll_launches` responde al recogerlo (ADR 0015). Sin ranura libre el
  lanzamiento recibe `Limit`. Con `Request::DETACH` (`sys::launch::spawn`,
  `spawn` en el shell) el broker responde `Started` al crear el proceso y
  nadie espera su salida. Si el programa no está en el initrd, el broker
  lo busca instalado en AriFS (`apps/NOMBRE.aripkg`, ADR 0017): copia el
  paquete por fsd a un buffer DMA de preparación de 256 KiB reservado una
  vez, lo verifica con `aricode-pkg` (firma Ed25519 del manifiesto del
  paquete con la clave de desarrollo y SHA-256 del ELF y del manifiesto de
  aplicación), aplica la política al manifiesto y lo lanza con
  `process_spawn_from` (syscall 30: `MANAGE` sobre el `SPAWNER`, `READ`
  sobre el buffer, desplazamiento y longitud del ELF; el kernel copia los
  segmentos por el mapa físico y el buffer queda libre para el siguiente).
  Desde el sistema, `installer` (capacidad `install`: el espacio principal
  de fsd, solo para él) instala o actualiza `apps/NOMBRE.aripkg` desde
  `pkgs/` con un único `replace` tras verificar y comparar versiones (ADR
  0018). Si el kernel rechaza el `spawn` por cualquier
  motivo, los duplicados preparados se cierran (el kernel solo los retira al
  crear el proceso): antes cada nombre inexistente consumía una entrada de
  la tabla de init. El shell recibe el endpoint del broker en
  lugar del `SPAWNER`.
- fsd mantiene hasta 8 espacios de nombres concedidos: cada `Bind` (solo
  por el endpoint principal) crea o reutiliza un endpoint ligado a un
  prefijo, y cada `Share` (ADR 0016) uno ligado a un único fichero en modo
  lectura; el bucle lee el principal con 10 ms de espera y los concedidos
  sin espera. Desde un privado, `Open`/`Create` califican el nombre como
  `prefijo/nombre` (rechazando `/` en el nombre) y `List` filtra y recorta
  el prefijo. Desde un fichero compartido, `List` muestra solo ese fichero,
  `Open` solo lo abre a él, y `Create`, `Write`, `Replace`, `Flush`, `Bind`
  y `Share` responden `Denied`. El broker pide un `Share` por cada nombre
  de `read_files` del manifiesto y deniega el lanzamiento si el fichero no
  existe (`sandbox.shared_file`).

- La consola serie del kernel serializa cada mensaje con un spinlock sin
  deshabilitar interrupciones (`console::write_raw`): antes un `info!` del
  kernel y un `console_write` de un proceso recién creado se mezclaban
  carácter a carácter. Con interrupciones deshabilitadas solo se intenta el
  lock, para que un handler nunca espere a un hilo que no puede correr.

## Runtime WebAssembly (Fase 29)

- `aricode-wasm` analiza el binario una vez (`Module::parse`: tipos,
  importaciones, funciones, tabla, memoria, globales, exportaciones,
  elementos y datos) y ejecuta byte a byte con una pila de valores `u64` y
  una de etiquetas. Los fines de bloque se localizan saltando inmediatos al
  entrar en cada bloque; `br` recorta la pila a la altura de la etiqueta y
  conserva la aridad; la etiqueta implícita de la función hace de `return`.
  Límites: 64 llamadas anidadas, 16 K valores, 50 M pasos (`Fuel`). Toda
  carga, almacenamiento, `memory.copy` y `memory.fill` se comprueba contra
  el `&mut [u8]` que aporta el anfitrión; la coma flotante devuelve
  `Unsupported`. Los tests de host usan un ensamblador mínimo de módulos
  para cubrir aritmética, bucles, `if/else` con resultado, recursión,
  `br_table`, globales, `call_indirect`, traps y límites.
- `Module::parse` valida el módulo entero antes de devolverlo (ADR 0019:
  tipado de cada función con pila de operandos y marcos de control, índices
  de tipos, funciones, tabla, globales, exportaciones y `start`); lo que el
  intérprete no ejecuta se rechaza ahí. El perfil aceptado está en
  `docs/wasm-profile.md`.
- `wasm_runner` es una aplicación normal del broker: recibe `SYSTEM`, su
  directorio privado y `HARDWARE`; lee `demo.wasm` (xtask lo compila con
  `-C target-cpu=mvp -C target-feature=+sign-ext,+bulk-memory` y lo deja en
  `build/`, y `write_test_disk` lo copia a `wasm_runner/demo.wasm`), resuelve
  las importaciones por nombre (`aricode.print`, `aricode.uptime_ms`) y
  reserva la memoria lineal como buffer DMA dentro de su cuota. El módulo no
  tiene manera de nombrar nada que el runner no le haya importado.
- Los ficheros de AriFS pasan a 32 KiB de capacidad para alojar módulos.

## Criptografía (Fase 31)

- `aricode-crypto` no tiene dependencias ni `alloc`. Los hashes son
  incrementales (`update`/`finish`); Ed25519 usa cinco limbs de 51 bits para
  GF(2^255-19), coordenadas extendidas (fórmulas de la RFC 8032 §5.1.4),
  descompresión por raíz cuadrada con `(p-5)/8` y escalares módulo L por
  división larga binaria (lenta pero simple: unas pocas firmas por
  arranque). Verificación comprobada con los tres vectores de la RFC 8032
  además de firmas y claves alteradas y `S` no canónico.
- `kernel/src/random.rs`: `Rng` de ChaCha20 sembrado con SHA-256 de ocho
  valores de `rdrand` (si CPUID.1:ECX.30) y 64 muestras del TSC más el
  tick de arranque; cada `fill` mezcla otra muestra (`reseed`). `qemu64` no
  expone `rdrand`, así que en los tests la fuente es el TSC y el kernel lo
  dice. `system_info(RANDOM)` sirve hasta 256 bytes por llamada.
- xtask gana `keygen`, `sign` y `verify` (semilla y clave pública en hex);
  `keys/dev` guarda el par de desarrollo que firmará las imágenes de prueba.

## Actualización A/B (Fase 30)

- Disco de pruebas de 48 MiB: `ARI0` (AriFS), `SYSA`, `SYSB`, `UPDATE` (12
  MiB cada una) y `ARIBOOT` (32 KiB). El escenario elige el estado inicial
  con `disk = slots | esp-only | pending-b | b-exhausted | b-corrupt`;
  los escenarios de fallos de la ESP usan `esp-only`.
- `aricode-ab`: `BootState` (magia, activo, pendiente+intentos, contador,
  CRC de SHA-256 truncado), `choose`/`reject`/`confirm`/`schedule` como
  funciones puras, y `Image` (cabecera `ARISLOT` + contenedor con
  manifiesto firmado). Todo probado en host, incluidas firma ajena, kernel
  alterado y contenedor sin firma.
- AriBoot (`boot/src/slots.rs`) abre los `BlockIo` de disco entero con
  `GetProtocol` (una apertura exclusiva desconectaba el driver FAT de la
  ESP), analiza la GPT con `aricode-gpt`, lee `ARIBOOT`, aplica `choose`,
  lee y verifica la imagen del slot con la clave pública de `keys/dev`
  embebida, escribe el estado (intento consumido) y entrega kernel e initrd
  a `run`; si el pendiente falla, `reject` y el activo; si nada vale, ESP.
  Añade `boot_slot=X [boot_pending]` a la cmdline.
- init confirma el slot pendiente al final de los servicios de arranque:
  localiza `ARIBOOT` leyendo la GPT por el servicio de bloques y escribe
  `confirm(state, slot)`. `updater` hace el camino inverso con `schedule`.
- xtask quita la información de depuración de kernel e initrd (`strip
  --strip-debug` si existe): el initrd pasa de 4,8 MiB a 550 KiB y la
  imagen de sistema cabe en el slot.

- Correcciones de la auditoría `e9fdf38`: los descriptores de fsd
  pertenecen al endpoint que los abrió; el broker cierra sus duplicados con
  un guard en cualquier salida; init confirma el slot solo al final del
  arranque (`init_fail_late` prueba el camino de error); AriBoot no arranca
  un pendiente cuyo intento no pudo persistir (`unpersisted`); el intérprete
  WASM acota memoria al tamaño actual, la inicializa a cero y ejecuta
  `start`; `keygen` crea la semilla con modo 0600.

## SMP (Fase 32)

```text
kernel/src/cpu.rs              Cpu por procesador (GDT, TSS + pila #DF, hilo actual, quantum, scratch syscall), GS base
kernel/src/arch/x86_64/smp.rs  trampolín 0x8000, tablas de arranque, INIT-SIPI, ap_entry, TLB shootdown, halt_others
kernel/src/arch/x86_64/apic.rs ICR: send_ipi, broadcast_ipi, send_init, send_startup; vectores 0x21 RESCHED, 0x22 TLB_FLUSH, 0x23 HALT
libs/sched                     cola compartida, current/idle por CPU, afinidad, despertares pendientes
```

- Arranque: tras `task::init` el BSP copia el trampolín a `0x8000` (memoria
  baja reservada por el PMM y nunca devuelta al reclamar la de arranque),
  construye tres frames de tablas (copia del PML4 del kernel + identidad de
  2 MiB) y, por cada LAPIC de la MADT distinto del suyo, crea el hilo idle de
  esa CPU (su pila es la del arranque del AP), escribe los parámetros
  (PML4, rsp, `ap_entry`, índice) y envía INIT, 10 ms, SIPI, 1 ms, SIPI si
  aún no está en línea, y espera hasta 500 ms. El AP recorre modo real ->
  protegido -> largo (PAE, `EFER.LME|NXE`), salta a `ap_entry`, carga el CR3
  del kernel y CR0/CR4 del BSP, instala su GDT/TSS, la IDT compartida,
  `syscall`, `IA32_GS_BASE`, su LAPIC (timer con la cuenta calibrada por el
  BSP), adopta su idle y habilita interrupciones. Las tablas de arranque se
  liberan al terminar.
- Por CPU: `Cpu` estático con `self_ptr` en `gs:[0]`; `cpu::current_tid()`
  y `cpu::id()` son un solo acceso relativo a GS (atómico frente a
  migraciones). `swapgs` en `syscall_entry`/`sysretq`, en
  `interrupt_common` cuando `cs & 3 == 3` (a la entrada y a la salida) y en
  `usermode::enter` (con `cli` antes). `TSS.rsp0` y el scratch de `syscall`
  se fijan en cada cambio de contexto en la CPU que conmuta.
- Planificación: solo el BSP llama a `time::tick` y `wake_due`; los APs solo
  consumen quantum. Cuando un hilo pasa a listo (`spawn`, `wake`, dormidos
  que despiertan) y otra CPU está en su idle se le envía `RESCHED`; el bucle
  idle comprueba la cola con IF = 0 y duerme con `sti; hlt` (sin ventana). Un
  hilo migra libremente salvo afinidad (`spawn_on`, `set_affinity`).
- TLB: los slots de pila de hilo no se reutilizan y un proceso muerto ya no
  tiene su CR3 en ninguna CPU (cada cambio a otro espacio recarga CR3), así
  que las rutas normales de liberación siguen con `invlpg` local. Para
  desmapeos de la mitad del kernel en uso por otras CPUs existe
  `vmm::flush_page` (local + `TLB_FLUSH` con confirmación); el receptor
  invalida la página o, con dirección 0, todo el TLB alternando `CR4.PGE`.
- Pánico: `halt_others` envía `HALT` a las demás CPUs antes del volcado, que
  indica la CPU; las excepciones fatales también.
- `system_info(CPUS)` y `cpus` en arish. Pruebas: `test=smp` y escenarios
  `smp.*`; `cargo xtask test --cpus N` como estrés de toda la suite.

## IOMMU (Fase 33)

```text
libs/acpi                      DMAR: DRHD (ámbitos, INCLUDE_PCI_ALL), RMRR
libs/iommu                     registros, Capability/ExtendedCapability, RootEntry, ContextEntry, SecondLevel (3/4 niveles), FaultRecord
kernel/src/iommu.rs            unidades, dominios por proceso, attach/map_region/unmap_everywhere/release, interrupción de fallo (vector 0x24)
```

- Arranque: tras enumerar PCI, `iommu::init` mapea los registros de cada
  DRHD sin caché, elige los niveles de paginación (4 si SAGAW lo permite, si
  no 3, el mínimo entre unidades), programa la tabla raíz (`RTADDR` +
  `SRTP`), la interrupción de fallo (MSI al LAPIC del BSP, vector 0x24),
  invalida contextos e IOTLB y activa la traducción (`TE`). Si la unidad no
  es coherente con la caché (`ECAP.C = 0`, como en QEMU) cada entrada
  escrita se `clflush`ea.
- Dominios: uno por proceso, creado al primer `dma_alloc`, `mmio_map` de un
  DMA o mapeo de un dispositivo; DID desde 1 con lista de libres. Las
  tablas de segundo nivel mapean IOVA = física con R+W en páginas de 4 KiB.
- Asignación: los objetos MMIO que `pci::claim` entrega (configuración y
  BARs) llevan el `source_id` de la función; al mapear uno, el kernel escribe
  la entrada de contexto de esa función (tabla de contexto del bus creada
  bajo demanda) apuntando a las tablas del dominio del proceso, mapea sus
  RMRR y hace una invalidación global en la unidad.
- Liberación: `DmaBuffer::drop` retira la región de todos los dominios
  antes de devolver los frames; `release_address_space` destruye el dominio
  del proceso (entradas de contexto a no presente, invalidación, tablas al
  PMM).
- Fallos: la unidad escribe los FRCD y dispara la MSI; el handler los
  decodifica (`bus:dev.fn`, lectura/escritura, dirección, motivo), los
  limpia y los cuenta; `system_info(IOMMU)` devuelve unidades, dominios y
  fallos.
- QEMU: `-device intel-iommu,aw-bits=48` antes de los dispositivos y
  `iommu_platform=on` en cada virtio; el transporte acepta
  `VIRTIO_F_ACCESS_PLATFORM` siempre que se ofrezca. Orden de locks:
  `PROCESSES -> IOMMU -> PMM`; nunca al revés.
- Pruebas: `test=iommu` (`dma_rogue`) y `iommu.enabled`; con
  `--cpus 4` toda la suite corre traducida en SMP.

## USB HID sobre xHCI (Fase 25, segunda parte)

```text
libs/xhci                      estructuras xHCI/USB (sin hardware)
libs/input                     HidBootKeyboard / HidBootMouse: informes de arranque -> eventos evdev
services/usb-input             usbd: controlador xHCI, enumeración, endpoints de interrupción (servicio de entrada) y masivos (almacenamiento bulk-only como servicio de bloques, ADR 0023)
```

- init lanza `usb_input` por cada función PCI de clase `0c.03` con el contrato
  de driver (servicio, cfg, hardware, IRQ MSI-X, BAR0) más `SYSTEM`. El driver
  comprueba CAPLENGTH/HCIVERSION (los registros de capacidad se leen por
  palabras de 32 bits: QEMU no atiende accesos de 16 bits) y, si no es un
  xHCI, sale con 0 sin molestar.
- Memoria: un buffer DMA de 64 KiB repartido con un asignador lineal
  (DCBAA, scratchpads, anillo de comandos de 16 TRB, anillo de eventos de
  32 TRB, ERST, contextos de dispositivo e input context, anillos de
  transferencia de 16 TRB e informes). Con el IOMMU activo, todo ese buffer
  es lo único que el controlador puede tocar.
- Enumeración por puerto con dispositivo conectado: reset del puerto si no
  está habilitado, `Enable Slot`, `Address Device` con EP0 al tamaño de
  paquete por defecto de la velocidad, `GET_DESCRIPTOR` de dispositivo
  (`Evaluate Context` si el tamaño de paquete real difiere) y de
  configuración, `SET_CONFIGURATION`, por cada interfaz HID de arranque
  `SET_PROTOCOL(0)` y `SET_IDLE(0)`, `Configure Endpoint` con el endpoint
  de interrupción IN y cuatro `Normal` TRB en vuelo por endpoint.
- Eventos: el anillo de eventos se consume con el bit de ciclo; los
  `Transfer Event` se casan con el TRB por su dirección física, el informe
  se decodifica (teclado: diferencias de modificadores y usages; ratón:
  botones, dx/dy, rueda) y el TRB se vuelve a encolar. Los `Port Status
  Change` solo limpian los bits de cambio.
- Un único dispositivo de entrada para todo el controlador
  (`USB HID Keyboard+Mouse`); `input_client` y el compositor enrutan por
  tipo de evento, no por dispositivo.
- Selección y publicación (auditoría 4836023): `pci_info_ext` expone la
  interfaz de programación y init solo entrega a `usb_input` los `0c.03`
  con interfaz `0x30`; cualquier servicio de entrada se publica cuando
  responde a `Info` (`input_service_ready`), y `InputClient` acota sus
  esperas para que un servicio muerto no bloquee al cliente. El descriptor
  de dispositivo se lee primero en 8 bytes para fijar el tamaño de paquete
  de EP0 con `Evaluate Context`; los intervalos FS/LS se redondean hacia
  abajo (3..=10).

## Supervisión de servicios (etapa 1, ADR 0011)

```text
libs/supervisor                estados, generaciones, presupuesto de reinicios con espera creciente, plazo de arranque
services/init                  Supervisor: Resident {kind, probe, endpoint, process, Service}; poll en el bucle del shell
sdk/proto::status              estado por servicio (protocolo 7) en el endpoint del broker; sdk::status::service_status
```

- init registra cada servicio residente (`virtio_blk`, `fsd`, `virtio_net`,
  `virtio_input`, `usb_input`) con su forma de relanzarlo (`Kind`), su sonda
  (`Probe`) y su política; `launch` crea el proceso, anota la generación y
  espera a que sirva; `poll` (en cada vuelta del bucle del shell) detecta
  muertes con `process_wait(0)`, aplica el presupuesto y relanza cuando toca,
  reclamando de nuevo la función PCI si el driver anterior murió.
- El endpoint del servicio pertenece a init y se entrega duplicado a cada
  generación: los clientes no cambian de handle.
- `restart_storm` (cmdline) reinicia 100 veces `crashy_service` con espera
  de 1 a 8 ms, comprobando que fsd responde entre medias y que la memoria
  libre vuelve al nivel anterior (`services.restart_storm`);
  `shell.services_detail` cubre el estado consultable.

## Plazos y correlación IPC (etapa 1B, ADR 0012)

- `aricode_sys::rpc::call`: un endpoint de respuesta por petición (creado y
  cerrado en la llamada), envío y recepción con plazo, handles extra que se
  consumen o se cierran si el envío falla. Lo usan todos los clientes del
  SDK; `launch::run` es el único que espera la respuesta sin plazo (es la
  salida del programa).
- `test=ipc_late` (`ipc.late_reply`): `late_server` contesta la primera
  petición 800 ms tarde; `late_client` vence a los 200 ms, pide otra y
  recibe la suya, nunca la tardía, que muere en el endpoint ya cerrado.

## Hardware real (Fase 34, preparación)

- `console_input` también lee el teclado PS/2 del i8042: inicialización
  (puertos deshabilitados, configuración con IRQ1 y traducción, autotest,
  puerto 1, `0xF4`), IRQ1 en el vector 0x41 y decodificación con
  `aricode_input::ScancodeSet1` + `Keyboard` al mismo anillo que COM1. Es la
  consola de un portátil sin serie; los teclados de verdad siguen en ring 3.
  Sin controlador (estado 0xFF) se informa y se sigue.
- `smp=off` e `iommu=off` en la cmdline saltan `smp::start_aps` e
  `iommu::init` con una traza; `test=console_ps2` prueba el teclado con
  entrada QMP.
- `iommu::init` crea dominios identidad (claves `u32::MAX - n`) para los
  dispositivos con RMRR y para los controladores gráficos (RMRR +
  framebuffer GOP) antes de activar `TE`, y solo después enciende la
  traducción.
- `cargo xtask usb-image`: `image::write_usb_image` construye la ESP con
  `aricode-fat` (64 MiB, clústeres de 512 bytes, LFN) a partir del directorio
  ESP del escenario, más ARI0, SYSA (imagen firmada), SYSB/UPDATE vacías y
  ARIBOOT; `disk = "usb-image"` arranca un escenario desde esa imagen sin el
  disco `fat:` de QEMU. Guía en `docs/hardware.md`.
- Primer arranque en el portátil (2026-10-10): AriBoot abre el GOP sin
  exclusividad (`OpenProtocolAttributes::GetProtocol`), igual que los
  `BlockIO`; la apertura exclusiva desconectaba el driver de consola del
  firmware y en hardware real las trazas se cortaban tras la línea de slots.
  Para la ventana sin trazas entre `ExitBootServices` y la consola del
  kernel, `FramebufferInfo::beacon` (`Beacon` en `boot-protocol`, que también
  centraliza `encode` del formato de píxel) pinta cuadrados en la fila
  superior: blanco antes de salir de boot services, verde antes del salto y
  naranja en `kernel_entry`, y cian por cada paso del cargador tras la
  cmdline; la consola los borra al acoplarse. Tabla de lectura en
  `docs/hardware.md`.
- Segunda y tercera foto: el firmware se para en la primera `GetMemoryMap`,
  justo tras reservar con un tipo de memoria propio. ADR 0021: AriBoot pasa a
  `LOADER_DATA`, BootInfo v4 describe lo que el kernel conserva y
  `boot/src/memmap.rs` lee el mapa con llamadas directas trazadas.
- Cuarta foto (2026-10-10): **primer arranque completo en hardware**. El
  kernel enumera 17 funciones PCI (ECAM en `0xf8000000`), no hay DMAR, init
  pasa sus pruebas de arranque y termina en `ARICODEOS_KERNEL_READY`. Lo que
  falta: el xHCI (`8086:1e31`) solo tiene MSI, no MSI-X, y el driver no
  recibía IRQ (ADR 0022 lo resuelve); los EHCI no tienen driver; sin driver
  de bloques para el pendrive no hay fsd y init no lanza el shell.
- Quinta foto: `reclamado (MSI)` y el driver cae en la reserva DMA: el
  controlador pide scratchpads (QEMU ninguno) y el buffer fijo de 64 KiB no
  los cabía. `Params::dma_arena_bytes` lo dimensiona y el driver nombra el
  paso que falla.
- Séptima y octava foto: el pendrive se lee por USB, `fs_client` fallaba
  en el flush (el pendrive rechaza `SYNCHRONIZE CACHE`; usbd lo tolera) y
  después **shell interactivo en el portátil** con el teclado PS/2. El
  retroceso pintaba `?`: la consola de framebuffer no entendía `\x08` y
  ahora mueve el cursor una celda.
- Sexta foto: xHCI listo (8 puertos, 32 slots, 16 scratchpads), servicio de
  entrada ok, `ARICODEOS_KERNEL_READY`; ningún dispositivo en los puertos
  porque en los Intel 7 series los puertos USB quedan en los EHCI hasta que
  el sistema los enruta al xHCI (registros XUSB2PR/USB3_PSSEN). ADR 0023:
  usbd enruta los puertos, pide la cesión al firmware y sirve el pendrive
  como servicio de bloques; sin virtio-blk, init monta fsd sobre él
  (escenario `usb.storage`).
- Octava y novena foto: shell interactivo sobre el pendrive con teclado
  PS/2, cursor e historial. Red: ADR 0024 saca la pila a `ari-net-core` y
  añade `rtl-net` para la Realtek `10ec:8136`; sin virtio-net, init lo
  lanza y omite los clientes de prueba del respondedor de QEMU.
- Décima foto: `rtl_net` no arrancaba ("el ELF no cabe en la region de
  usuario"): faltaba su `build.rs` y se enlazó en la dirección por defecto.
  `cargo xtask build` comprueba ahora el punto de entrada de cada programa
  del initrd, y `diag NOM` + `cargo xtask fs-cat` dan un canal de
  diagnóstico sin fotos.

## Pila de arranque y consola (auditoría 3eb4965)

- El kernel arranca sobre la pila de 64 KiB de AriBoot hasta que `vmm::init`
  crea la suya. Nada grande puede vivir en esa pila: la consola de
  framebuffer (rejilla de 32 KiB) es un `static` construido con
  `Console::empty()` (`const`) al que se acopla la pantalla con `attach`.
  `check_boot_stack` mide lo usado justo antes de cambiar de pila y exige
  16 KiB de margen; AriBoot y el kernel anuncian el SHA-256 del initrd para
  detectar corrupciones entre ambos.
- `wait_until` retira un proceso solo cuando `release_address_space` lo ha
  marcado `reaped` (espacio y dominio DMA liberados); el IOMMU guarda un
  propietario único por función PCI y `release` no toca contextos que ya
  son de otro dominio.

## Deuda conocida

- No hay reinicio automático tras `updater` ni prueba de dos arranques
  encadenados en un mismo QEMU; la actualización llega por xtask, no por red.
- El generador del kernel solo tiene `rdrand` y TSC como fuentes.
- El intérprete WASM no valida el módulo antes de ejecutar (confía en el
  productor; todo se comprueba en ejecución) y no tiene coma flotante.
- La política del broker es una constante; no hay cuotas de CPU ni de
  handles por manifiesto y el heap de usuario sigue siendo estático.
- La GUI no mueve ni redimensiona ventanas, repinta la pantalla completa
  en cada frame y no tiene terminal gráfico; el shell sigue en la serie.
- Sin xHCI/USB HID: solo dispositivos virtio-input; el shell sigue leyendo
  de la consola serie.
- HTTP sin TLS, sin redirecciones ni `keep-alive`; cuerpos limitados al
  buffer del llamador.
- `netd` no renueva la concesión DHCP ni fragmenta IPv4; un socket UDP
  guarda un solo datagrama sin recoger; TCP reenvía desde `snd_una` sin
  SACK ni estimación de RTT. Un cliente que envía a un servicio muerto se
  queda bloqueado en la cola del endpoint (no hay aviso de cierre).
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
- El IOMMU invalida globalmente tras cada cambio y no remapea
  interrupciones; sin variante AMD-Vi ni dominios compartidos entre
  procesos.
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
- Sin prioridades ni balanceo de carga: round robin sobre una cola
  compartida entre CPUs, con afinidad opcional. Las IRQ de dispositivo van
  siempre al BSP.
- Los slots de pila no se reutilizan (uno por tid); el espacio virtual
  reservado permite ~2^31 hilos, así que no es un límite práctico aún.
- El LAPIC también queda cubierto por el mapa físico lineal con caché WB;
  solo se accede por la ventana sin caché, pero es un alias a resolver
  cuando se reserve MMIO fuera del mapa lineal.
- Sin I/O APIC ni MSI: ninguna IRQ de dispositivo todavía (Fase 17).
- El heap nunca devuelve trozos al PMM ni compacta; el slab/size-class de la
  sección 16 llegará cuando haya métricas que lo pidan. Las tablas intermedias
  creadas durante un crecimiento fallido tampoco se devuelven (se reutilizan).
- Las tablas del cargador se recuperan en `reclaim_boot_memory` (ADR 0021:
  `page_tables_base/size` en `BootInfo`); las páginas de `kernel_ranges()`
  se vuelven a reservar tras liberar la memoria de arranque.
- La invalidación de TLB sigue siendo explícita en cada ruta que desmapea:
  local (`invlpg`) donde ninguna otra CPU pudo usar la traducción (heap al
  deshacer un crecimiento, pilas de hilo que no se reutilizan) y con
  shootdown (`vmm::flush_page`) en la autoprueba del VMM y `test=smp`; no
  hay un mecanismo genérico que decida solo.
- El loader depende de la crate `uefi` (ver ADR 0006).
