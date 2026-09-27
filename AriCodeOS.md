# AriCodeOS

## Documento maestro de arquitectura, especificaciones técnicas y plan de desarrollo

**Estado:** 0.0.7 completada (2026-09-26): init, drivers en espacio de usuario, PCIe  
**Versión del documento:** 0.2  
**Nombre del sistema:** AriCodeOS  
**Arquitectura inicial:** x86-64  
**Modelo de arranque:** UEFI  
**Lenguaje principal previsto:** Rust (`no_std`)  
**Lenguajes auxiliares:** Assembly x86-64 y, solo cuando sea necesario, C  
**Plataforma inicial de pruebas:** QEMU + OVMF sobre Debian 13  
**Hardware real:** portátil antiguo dedicado a pruebas cuando el sistema alcance una fase suficientemente estable

---

# 1. Visión

AriCodeOS nace con la intención de crear un sistema operativo moderno desde cero, sin utilizar Linux, Windows, BSD u otro kernel como base de ejecución.

El proyecto no pretende ser inicialmente un reemplazo general de Windows o Linux. Su primera meta es construir un núcleo moderno, pequeño, seguro, verificable y diseñado alrededor del hardware actual.

Los principios fundamentales serán:

- eliminar compatibilidad heredada que no aporte valor;
- minimizar el código ejecutado con privilegios máximos;
- aislar componentes y drivers;
- utilizar seguridad por capacidades;
- reducir clases completas de vulnerabilidades mediante memoria segura;
- diseñar actualizaciones atómicas y recuperables;
- asumir arquitectura de 64 bits desde el principio;
- diseñar para CPU, GPU, NPU y aceleradores futuros;
- mantener una arquitectura modular y testeable;
- evitar dependencias arquitectónicas innecesarias de POSIX;
- permitir compatibilidad con software antiguo mediante capas aisladas, no contaminando el kernel.

La filosofía puede resumirse así:

> AriCodeOS no intenta ser un Linux más pequeño. Intenta replantear qué debería ser un sistema operativo moderno.

---

# 2. Objetivos principales

## 2.1 Objetivos funcionales

AriCodeOS deberá llegar progresivamente a proporcionar:

1. Arranque UEFI nativo.
2. Kernel propio de 64 bits.
3. Gestión de memoria física y virtual.
4. Interrupciones y temporizadores.
5. Ejecución multitarea.
6. Separación kernel / espacio de usuario.
7. Procesos aislados.
8. IPC seguro.
9. Modelo de capacidades.
10. Drivers ejecutados fuera del kernel siempre que sea posible.
11. Almacenamiento persistente.
12. Sistema de archivos.
13. Red.
14. Entrada de teclado y ratón.
15. Sistema gráfico.
16. Runtime de aplicaciones.
17. Aplicaciones nativas.
18. Aplicaciones portables mediante WebAssembly u otra tecnología equivalente.
19. Actualización atómica A/B.
20. Compatibilidad opcional con software Linux mediante aislamiento.

---

# 3. No objetivos iniciales

Durante las primeras versiones AriCodeOS NO intentará:

- ejecutar software Windows directamente;
- ejecutar software Linux directamente;
- implementar POSIX completo;
- soportar hardware de 16 o 32 bits;
- soportar BIOS clásico;
- soportar floppy, ISA, IDE antiguo u otros buses heredados;
- competir con Linux en cantidad de drivers;
- implementar una interfaz gráfica completa antes de estabilizar el kernel;
- implementar decenas de arquitecturas CPU simultáneamente;
- optimizar prematuramente componentes todavía inestables.

---

# 4. Arquitectura inicial soportada

## 4.1 CPU

Primera arquitectura:

```text
x86-64 / AMD64
```

Requisitos previstos:

- CPU de 64 bits;
- paginación de 64 bits;
- NX bit;
- SSE2 como mínimo;
- APIC;
- UEFI;
- ACPI preferentemente disponible;
- soporte SMP en fases posteriores.

Arquitecturas futuras:

```text
AriCodeOS
├── x86_64
├── aarch64
└── riscv64
```

No se implementarán ARM64 o RISC-V hasta que la separación entre código genérico y código dependiente de arquitectura esté consolidada.

---

# 5. Arquitectura conceptual

```text
+------------------------------------------------------+
|                    Aplicaciones                      |
|       Native Apps | WASM Apps | Compatibility        |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|                    Runtime / SDK                     |
| Capabilities | IPC API | App sandbox | Services API |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|                 Servicios de usuario                 |
| Storage | Network | Audio | GUI | Device Manager     |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|             Drivers en espacio de usuario            |
| NVMe | USB | HID | Network | Audio | GPU | etc.      |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|                AriCodeOS Microkernel                 |
| Scheduler | VM | IPC | IRQ | Capabilities | SMP     |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|                       Hardware                       |
+------------------------------------------------------+
```

---

# 6. Filosofía del kernel

AriCodeOS utilizará inicialmente una filosofía de **microkernel pragmático**.

El kernel debe contener únicamente lo imprescindible para administrar recursos críticos.

## 6.1 Responsabilidades del kernel

El kernel será responsable de:

- cambio de contexto;
- planificación de CPU;
- gestión de espacios de direcciones;
- memoria virtual;
- memoria física básica;
- interrupciones;
- temporizadores;
- IPC;
- capabilities;
- syscalls mínimas;
- sincronización fundamental;
- control SMP;
- primitivas necesarias para delegar hardware.

## 6.2 Fuera del kernel

Siempre que sea técnicamente razonable deberán vivir en espacio de usuario:

- almacenamiento;
- USB;
- red;
- audio;
- input;
- filesystem;
- servicios del sistema;
- compositor gráfico;
- gestores de dispositivos;
- demonios de compatibilidad.

Objetivo:

```text
fallo de driver != fallo del kernel
```

Un driver podrá ser finalizado y reiniciado sin necesariamente provocar un kernel panic.

---

# 7. Lenguajes

## 7.1 Rust

Lenguaje principal.

Motivos:

- memory safety;
- ownership;
- ausencia de garbage collector obligatorio;
- adecuado para código `no_std`;
- abstracciones de coste cero;
- sistema de tipos fuerte;
- buen soporte para concurrencia segura;
- reducción de use-after-free, double-free y muchos buffer overflows.

El kernel se compilará inicialmente con:

```text
#![no_std]
#![no_main]
```

## 7.2 Assembly

Se utilizará únicamente donde sea necesario:

- entrada inicial al kernel;
- manipulación de registros especiales;
- tablas de interrupciones;
- context switch;
- syscalls;
- instrucciones de CPU específicas;
- arranque de CPUs secundarias;
- operaciones imposibles o poco prácticas desde Rust seguro.

## 7.3 C

No será lenguaje principal.

Podrá admitirse de forma limitada para:

- integración con librerías externas auditadas;
- prototipos específicos;
- compatibilidad con alguna herramienta o driver experimental.

La política será evitar introducir C en el Trusted Computing Base si existe una alternativa razonable.

## 7.4 Valoración: Rust frente a AriCode como lenguaje del kernel

Existe un compilador propio, **AriCode** (`~/git-proyect/aricoderoot`), y era
obligado valorar si el kernel debía escribirse con él en lugar de con Rust.
La valoración se hizo el 2026-09-26 sobre el código real del compilador, no
sobre la especificación.

### Qué es AriCode hoy

- Compilador AOT escrito en C (~30 400 líneas) que emite código máquina
  x86-64 directamente desde el AST, sin IR, sin LLVM, sin ensamblador y sin
  linker, con un escritor ELF propio.
- Un único target: `x86_64-linux` (`aricode/aricode.toml`). No existen
  `--target`, `--emit-asm` ni salida PE32+/objeto relocalizable.
- Todo binario generado depende del ABI del kernel Linux: el prólogo fijo
  reserva un arena con `mmap` en `0x200000000000` y termina con
  `syscall exit(60)`. Hay ~111 puntos de emisión de syscalls (write, read,
  mmap, open, socket, epoll, clone, futex, wait4).
- Sin GC ni ownership real: la memoria es un bump arena. Arrays y "structs"
  son handles enteros opacos.
- La especificación describe ownership/borrowing, `unsafe`, punteros crudos,
  FFI `extern "C"` y traits, pero **nada de eso está implementado**: el lexer
  no reconoce `unsafe`, `extern`, `asm` ni `Box`.
- Sin punteros desreferenciables, sin acceso volatile, sin control de layout
  (`repr(C)`), sin inline assembly, sin linker scripts, sin entry point ni
  dirección de carga configurables (fijos en `0x400000 + 120`, un solo
  `PT_LOAD` sin secciones ni símbolos).
- Sin FFI en ninguna dirección: no puede llamar a C/Rust/asm ni ser enlazado
  desde ellos, porque no produce ficheros objeto.
- Madurez: v0.1.0, 216 commits de un único autor entre 2026-04-11 y
  2026-06-28, sin CI, con binarios compilados en el repositorio y tests
  cuyo recuento difiere entre documentos (23, 39+55, 85). Su foco actual es
  inferencia LLM cuantizada (int8/int4) y criptografía.

### Comparativa para escribir un microkernel

| Requisito del kernel                         | Rust `no_std`                    | AriCode (estado actual)                    |
|----------------------------------------------|----------------------------------|--------------------------------------------|
| Binario freestanding sin OS debajo           | Sí (`x86_64-unknown-none`)       | No: prólogo hace `mmap` y `exit` Linux     |
| Aplicación UEFI (PE32+)                      | Sí (`x86_64-unknown-uefi`)       | No: solo ELF Linux                         |
| Entry point / dirección de carga propios     | Sí (linker script, LLD)          | No: fijos en el escritor ELF               |
| Punteros crudos, volatile, MMIO              | Sí (`*mut T`, `read_volatile`)   | No                                         |
| Layout de estructuras controlado             | Sí (`repr(C)`, `repr(u8)`…)      | No                                         |
| Inline assembly / instrucciones privilegiadas| Sí (`asm!`, `naked` fns)         | No (solo `rdtsc` como builtin)             |
| Enlazado con ensamblador externo             | Sí (objetos ELF, LLD)            | No: no produce objetos ni símbolos         |
| Memory safety en el TCB                      | Sí (ownership verificado)        | Parcial: solo bounds checks + arena        |
| Concurrencia verificada (`Send`/`Sync`)      | Sí                               | No                                         |
| Depuración con símbolos (GDB)                | Sí (DWARF)                       | No: sin tabla de símbolos                  |
| Objetivo multi-arquitectura futuro           | Sí (aarch64, riscv64)            | No: solo x86-64                            |
| Mantenimiento / ecosistema                   | Comunidad, `uefi`, `x86_64`…     | Una persona, 3 meses, sin CI               |

### Qué costaría hacer viable AriCode para el kernel

Como mínimo habría que añadir al compilador: un modo freestanding sin
syscalls ni arena; salida PE32+ o ELF relocalizable enlazable; punteros
reales, volatile y control de layout; inline asm o builtins para
`cli/sti/hlt/lgdt/lidt/CRn/MSR/in/out`; convenciones de llamada para
interrupt frames; y FFI o ficheros objeto para convivir con asm/Rust. Todo
ello sobre un backend sin IR donde cada builtin son bytes emitidos a mano.
Es, en la práctica, escribir un compilador de sistemas nuevo antes de
escribir el kernel.

### Decisión

```text
Kernel, bootloader, drivers y servicios base -> Rust no_std
AriCode                                       -> NO forma parte del TCB
```

Razones:

1. El kernel necesita hoy las capacidades de la tabla; AriCode no las tiene
   y no están en su hoja de ruta a corto plazo.
2. Depender de AriCode acoplaría el calendario del SO al del compilador, y
   ambos proyectos tienen un único desarrollador.
3. Rust ofrece justamente las garantías que la sección 51 exige para el
   TCB (memory safety verificada, `unsafe` acotado y auditable).
4. Renunciar a AriCode en el kernel no lo excluye del sistema: su lugar
   natural está encima del kernel, como se detalla en la sección 105.

Esta decisión se registrará como `docs/decisions/0001-use-rust.md` y deberá
revisarse si AriCode incorpora un backend freestanding y FFI reales.

---

# 8. Toolchain de desarrollo

Sistema anfitrión inicial:

```text
Debian 13 (trixie), x86-64
```

Estado de la estación de desarrollo tras la instalación del 2026-09-26:

```text
rustup 1.29.1          -> ~/.cargo/bin (PATH añadido en .bashrc y .profile)
rustc 1.98.1 stable    -> vía rustup; el rustc 1.85 de Debian queda detrás en PATH
targets                -> x86_64-unknown-none, x86_64-unknown-uefi
componentes            -> rust-src, llvm-tools, rustfmt, clippy
qemu-system-x86_64     -> 10.0.13 (Debian)
OVMF                   -> /usr/share/OVMF/OVMF_CODE_4M.fd, OVMF_VARS_4M.fd
lld                    -> 19.1.7
gdb                    -> 16.3
nasm, clang, git, make -> instalados
```

Prueba de humo realizada: una aplicación UEFI mínima en Rust `no_std`
(target `x86_64-unknown-uefi`, sin crates externas) arrancó en QEMU + OVMF,
escribió por consola y terminó QEMU mediante `isa-debug-exit` con el código
esperado. La cadena firmware -> PE32+ propio -> salida -> exit code para CI
está validada.

El `rustc` empaquetado por Debian no permite añadir targets ni usar
`build-std`; por eso se usa rustup y la versión se fijará con
`rust-toolchain.toml`.

Herramientas:

```text
Rust
rustup
Cargo
LLVM / Clang
LLD
NASM
QEMU
OVMF
GDB
Git
Make o cargo xtask
Python/Node solo para tooling externo si resulta útil
```

Paquetes en Debian 13:

```bash
sudo apt install \
  qemu-system-x86 \
  ovmf \
  nasm \
  clang \
  lld \
  gdb \
  git \
  make
```

OVMF queda instalado en `/usr/share/OVMF/` (`OVMF_CODE_4M.fd` y
`OVMF_VARS_4M.fd`). El runner de QEMU deberá copiar `OVMF_VARS` a un
fichero de trabajo para no modificar el original.

Instalación de Rust mediante rustup (sin desinstalar el paquete Debian,
pero asegurándose de que `~/.cargo/bin` precede a `/usr/bin` en `PATH`):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-none
rustup target add x86_64-unknown-uefi
rustup component add rust-src llvm-tools
```

La configuración exacta del target puede evolucionar hacia targets JSON personalizados cuando necesitemos controlar ABI, red-zone, features CPU, modelo de código y linker con precisión.

---

# 9. Modelo de desarrollo

AriCodeOS se construirá primero dentro de QEMU.

```text
Debian (estación de desarrollo)
  |
  +-- toolchain
  |
  +-- compila AriCodeOS
  |
  +-- genera imagen EFI
  |
  +-- QEMU + OVMF
          |
          +-- arranca AriCodeOS
```

Linux (Debian) NO ejecutará AriCodeOS internamente.

Linux solamente será la estación de desarrollo que:

- edita código;
- compila;
- genera imágenes;
- lanza QEMU;
- ejecuta tests;
- depura mediante GDB.

Una vez QEMU deja el control al kernel:

```text
AriCodeOS se ejecuta directamente sobre el hardware virtual.
```

---

# 10. Arranque

## 10.1 No utilizar BIOS legacy

AriCodeOS comenzará directamente con UEFI.

No se implementará:

- boot sector MBR tradicional;
- real mode como mecanismo principal;
- cargador BIOS heredado;
- transición manual 16 -> 32 -> 64 bits como requisito inicial.

UEFI permite comenzar desde un entorno moderno de 64 bits.

---

# 11. Bootloader propio

El objetivo final será controlar nuestro propio proceso de arranque.

Arquitectura prevista:

```text
Firmware UEFI
    |
    v
BOOTX64.EFI
    |
    +-- obtiene GOP framebuffer
    +-- obtiene ACPI RSDP
    +-- obtiene memory map
    +-- carga kernel ELF
    +-- reserva memoria
    +-- prepara BootInfo
    +-- ExitBootServices()
    |
    v
AriCodeOS Kernel
```

## 11.1 BootInfo

El bootloader deberá entregar al kernel una estructura de información.

Ejemplo conceptual:

```rust
pub struct BootInfo {
    pub memory_map: MemoryMap,
    pub framebuffer: FramebufferInfo,
    pub rsdp_address: u64,
    pub kernel_phys_base: u64,
    pub kernel_virt_base: u64,
    pub initrd_address: u64,
    pub initrd_size: u64,
}
```

La estructura real deberá ser versionada y poseer ABI estable.

---

# 12. Formato del kernel

Formato recomendado inicial:

```text
ELF64
```

Ventajas:

- estándar ampliamente documentado;
- secciones claras;
- symbols útiles para depuración;
- relocations;
- compatibilidad con toolchains maduras.

Posteriormente puede existir un formato propio para aplicaciones sin necesidad de cambiar el formato interno del kernel.

---

# 13. Entrada al kernel

Ejemplo conceptual:

```text
UEFI bootloader
      |
      v
kernel_entry(BootInfo*)
      |
      +-- deshabilitar dependencias UEFI
      +-- configurar CPU
      +-- instalar GDT
      +-- instalar IDT
      +-- inicializar memoria
      +-- inicializar consola temprana
      +-- habilitar interrupciones
      +-- iniciar scheduler
```

Primer milestone real:

```text
AriCodeOS booting...
```

mostrado desde código ejecutándose después de `ExitBootServices()`.

---

# 14. Gestión de memoria física

El bootloader entregará el mapa UEFI.

AriCodeOS clasificará regiones:

```text
FREE
KERNEL
BOOT
FIRMWARE
ACPI
MMIO
RESERVED
FRAMEBUFFER
INITRD
```

Primer allocator físico recomendado:

```text
bitmap page allocator
```

Tamaño de página inicial:

```text
4 KiB
```

Posteriormente se admitirán:

```text
2 MiB huge pages
1 GiB huge pages
```

cuando sea útil.

---

# 15. Memoria virtual

Cada proceso tendrá su propio espacio virtual.

Ejemplo conceptual:

```text
Virtual address space

0x0000000000000000
        |
        | user space
        |
0x00007FFFFFFFFFFF

        canonical gap

0xFFFF800000000000
        |
        | kernel space
        |
0xFFFFFFFFFFFFFFFF
```

La distribución exacta será definida cuando se implemente el VMM.

Protecciones obligatorias:

- páginas R/W/X diferenciadas;
- NX por defecto;
- W^X cuando sea posible;
- kernel no escribible desde user space;
- user pages inaccesibles por defecto desde contextos incorrectos;
- guard pages para stacks;
- ASLR en fases posteriores.

---

# 16. Heap del kernel

Evolución sugerida:

```text
fase inicial: bump allocator
      |
      v
linked-list allocator
      |
      v
slab / size-class allocator
```

El kernel no dependerá de un heap complejo durante las primeras etapas del arranque.

---

# 17. Interrupciones

En x86-64 se implementará:

- GDT;
- IDT;
- excepciones CPU;
- Local APIC;
- I/O APIC;
- MSI/MSI-X posteriormente.

Excepciones críticas:

```text
#DE Divide Error
#UD Invalid Opcode
#GP General Protection
#PF Page Fault
#DF Double Fault
#NMI
```

Un page fault deberá mostrar información diagnóstica detallada durante desarrollo.

Ejemplo:

```text
PANIC: PAGE FAULT
RIP: 0xFFFFFFFF80012345
CR2: 0x0000000000000040
ACCESS: WRITE
MODE: KERNEL
```

---

# 18. Temporizador

Primera implementación:

- LAPIC timer o mecanismo adecuado disponible;
- HPET puede utilizarse como referencia cuando sea necesario;
- TSC puede emplearse para medición después de validar estabilidad.

El timer proporcionará la base para:

- scheduler;
- timeouts;
- sleep;
- watchdogs;
- estadísticas.

---

# 19. Scheduler

Primer scheduler:

```text
Round Robin preemptivo
```

Más adelante:

```text
Priority scheduler
Fair scheduling
Real-time classes
CPU affinity
Energy-aware scheduling
Heterogeneous compute scheduling
```

Diseño futuro:

```text
TASK
 |
 +-- CPU general
 +-- GPU workload
 +-- NPU workload
 +-- DSP / accelerator
```

La meta a largo plazo es que AriCodeOS pueda administrar recursos heterogéneos y no solamente tiempo de CPU.

---

# 20. Threads y procesos

Entidades diferentes:

```text
Process
├── address space
├── capabilities
├── resources
└── Threads
    ├── registers
    ├── stack
    └── scheduling state
```

## 20.1 Thread

Contendrá como mínimo:

```text
ID
CPU registers
stack
state
priority
CPU affinity
process owner
```

Estados iniciales:

```text
READY
RUNNING
BLOCKED
SLEEPING
TERMINATED
```

---

# 21. Cambio de contexto

El context switch será una de las pocas zonas donde Assembly puede resultar necesario.

Deberá preservar como mínimo los registros definidos por el ABI interno del kernel.

El diseño deberá evitar guardar estado innecesario.

FPU/SIMD podrá utilizar lazy/eager context switching según resultados medidos y seguridad.

---

# 22. Syscalls

El ABI de syscall será propio.

No es necesario copiar Linux.

Primer conjunto conceptual:

```text
process_exit
thread_yield
ipc_send
ipc_receive
memory_map
memory_unmap
capability_clone
capability_drop
object_wait
object_signal
```

Debe mantenerse deliberadamente pequeño.

El kernel NO debería incluir cientos de syscalls especializadas si pueden modelarse mediante objetos y mensajes.

---

# 23. IPC

El IPC será pieza central.

Modelo deseado:

```text
client
  |
  | message
  v
endpoint
  |
  v
server
```

Propiedades:

- mensajes pequeños rápidos;
- transferencia opcional de páginas;
- transferencia controlada de capabilities;
- bloqueo o modo asíncrono;
- timeouts;
- cancelación;
- colas limitadas;
- protección frente a resource exhaustion.

IPC deberá ser medido desde etapas tempranas porque es crítico para un microkernel.

---

# 24. Modelo de objetos del kernel

Todo recurso crítico podrá representarse mediante un objeto kernel.

Ejemplos:

```text
Process
Thread
AddressSpace
MemoryObject
Endpoint
Interrupt
DeviceRegion
SharedMemory
Timer
```

Las aplicaciones no manejarán pointers directos a objetos kernel.

Utilizarán handles/capabilities.

---

# 25. Seguridad basada en capabilities

Principio:

> Tener identidad no implica tener acceso.

Una aplicación únicamente puede utilizar un recurso si posee explícitamente una capability adecuada.

Ejemplo:

```text
PhotoApp

CAP_READ  -> /photos
CAP_WRITE -> /photos/export
CAP_GPU   -> compositor

NO CAP_NET
NO CAP_MIC
NO CAP_RAW_DISK
```

Una capability deberá poder codificar:

- objeto;
- derechos;
- posibilidad de delegación;
- restricciones;
- lifetime.

Ejemplo conceptual:

```text
Capability {
    object: FileObject(1234)
    rights: READ
    delegable: false
}
```

---

# 26. Principio de mínimo privilegio

Por defecto una aplicación comienza prácticamente sin privilegios.

```text
APP
├── RAM propia          YES
├── CPU                 YES
├── network             NO
├── camera              NO
├── microphone          NO
├── arbitrary files     NO
├── raw devices         NO
└── kernel memory       NEVER
```

Los permisos se conceden mediante capacidades específicas.

---

# 27. Drivers en espacio de usuario

Arquitectura deseada:

```text
Application
     |
     v
System Service
     |
     v
Driver process
     |
     v
MMIO / IRQ capability
     |
     v
Hardware
```

Cada driver recibe únicamente:

- región MMIO necesaria;
- puerto I/O cuando corresponda;
- IRQ correspondiente;
- DMA buffers autorizados;
- canal IPC hacia su servicio.

No debe recibir acceso universal al kernel.

---

# 28. DMA e IOMMU

Cuando el hardware lo permita deberá utilizarse IOMMU.

Objetivo:

```text
device DMA
   |
   v
solo memoria autorizada
```

Sin IOMMU un dispositivo con DMA puede potencialmente escribir en memoria arbitraria.

Por ello el soporte VT-d/AMD-Vi/IOMMU será una característica de seguridad importante.

---

# 29. Device Manager

Servicio de usuario encargado de:

- enumerar dispositivos;
- identificar drivers compatibles;
- iniciar drivers;
- asignar capabilities de hardware;
- reiniciar drivers que fallen;
- administrar hotplug.

No necesita residir dentro del kernel.

---

# 30. PCI / PCI Express

El primer bus de dispositivos importante será PCI/PCIe.

Tareas:

1. localizar ECAM mediante ACPI MCFG;
2. enumerar buses;
3. leer vendor/device IDs;
4. interpretar BARs;
5. detectar capabilities PCIe;
6. asignar MMIO;
7. configurar MSI/MSI-X.

---

# 31. ACPI

Se utilizará ACPI para descubrir aspectos esenciales del hardware moderno.

Tablas relevantes:

```text
RSDP
XSDT
MADT
MCFG
FADT
HPET
DMAR / IOMMU
```

No se implementará AML completo inicialmente. `libs/acpi` parsea RSDP,
XSDT/RSDT, MADT y MCFG (Fase 16); HPET y DMAR quedan para cuando se usen.

Podrá incorporarse un intérprete AML posteriormente si resulta necesario.

---

# 32. Almacenamiento

Prioridad inicial:

```text
NVMe
```

Motivo:

- estándar moderno;
- ampliamente utilizado;
- evita dedicar recursos iniciales a IDE/PATA histórico.

SATA/AHCI puede añadirse después.

Orden previsto:

```text
RAM disk
  -> block device abstraction
  -> NVMe
  -> partitions
  -> filesystem
```

---

# 33. Abstracción de dispositivos de bloque

Interfaz conceptual:

```rust
trait BlockDevice {
    fn read_blocks(...);
    fn write_blocks(...);
    fn flush(...);
}
```

El filesystem nunca debería depender directamente de NVMe.

---

# 34. Sistema de archivos

Para las primeras fases pueden utilizarse dos estrategias:

## 34.1 Initramfs

Imagen incluida durante el boot con:

- proceso init;
- servicios básicos;
- pruebas;
- configuraciones.

## 34.2 Filesystem persistente

Primera versión puede ser deliberadamente simple.

Más adelante AriCodeOS podrá diseñar un filesystem propio con:

- checksums;
- snapshots;
- copy-on-write;
- metadata transaccional;
- recuperación ante errores;
- cifrado opcional.

No es prioritario inventar un filesystem avanzado antes de tener procesos, IPC y almacenamiento fiables.

---

# 35. Sistema base inmutable

Objetivo de seguridad:

```text
SYSTEM IMAGE -> READ ONLY
USER DATA    -> WRITABLE
APP DATA     -> SANDBOXED
```

El sistema base deberá poder verificarse criptográficamente.

---

# 36. Actualizaciones atómicas A/B

Diseño:

```text
SYSTEM_A
SYSTEM_B
USER_DATA
```

Ejemplo:

```text
actualmente activo: SYSTEM_A
actualización -> escribir SYSTEM_B
verificar firma
reiniciar
boot SYSTEM_B
```

Si el arranque falla:

```text
rollback -> SYSTEM_A
```

La actualización nunca debe modificar parcialmente el sistema actualmente en ejecución.

---

# 37. Secure Boot propio

Cadena conceptual:

```text
UEFI Secure Boot
      |
      v
AriCode Bootloader firmado
      |
      v
Kernel firmado
      |
      v
System image firmada
      |
      v
Servicios firmados
```

Durante desarrollo Secure Boot podrá permanecer desactivado.

Más adelante AriCodeOS podrá utilizar claves propias.

---

# 38. Red

Secuencia prevista:

```text
NIC driver
   |
Ethernet
   |
ARP / NDP
   |
IPv4 / IPv6
   |
ICMP
   |
UDP
   |
TCP
   |
DNS
   |
TLS en user space
```

La pila de red deberá vivir principalmente fuera del kernel.

---

# 39. Primer driver de red

En QEMU conviene utilizar inicialmente un dispositivo sencillo y bien documentado.

Opciones posibles durante desarrollo:

- virtio-net;
- Intel e1000 como alternativa educativa.

Para la arquitectura final se favorecerá virtio en entorno virtualizado y drivers nativos separados para hardware real.

---

# 40. Virtio

Virtio será especialmente útil en QEMU.

Drivers candidatos:

```text
virtio-blk
virtio-net
virtio-gpu
virtio-input
```

Permite construir las capas del SO antes de enfrentarse a la variedad extrema del hardware real.

---

# 41. Entrada

Fases iniciales:

```text
UEFI console -> solo boot
PS/2 emulado / virtio-input -> desarrollo
USB HID -> hardware moderno
```

A largo plazo el stack USB será imprescindible.

---

# 42. USB

Implementación futura por etapas:

```text
xHCI controller
    |
USB core
    |
Device enumeration
    |
Class drivers
├── HID
├── Mass Storage
├── Audio
└── otros
```

No se implementarán inicialmente controladores USB antiguos EHCI/OHCI/UHCI salvo necesidad concreta.

---

# 43. Gráficos

Primera salida gráfica:

```text
UEFI GOP framebuffer
```

Fases:

```text
1. framebuffer directo
2. font renderer
3. framebuffer console
4. 2D primitives
5. compositor
6. ventanas
7. input routing
8. GPU acceleration
```

El kernel no administrará ventanas.

---

# 44. Compositor gráfico

Proceso de usuario privilegiado mediante capabilities específicas.

```text
Apps
  |
  v
Compositor
  |
  v
GPU service / framebuffer
```

Cada aplicación renderizará sobre superficies propias.

El compositor decidirá qué región llega a pantalla.

---

# 45. Audio

El audio será un servicio de usuario.

```text
App
 |
 v
Audio Server
 |
 v
Audio Driver
 |
 v
Hardware
```

Permitirá mezclar varias aplicaciones sin entregar acceso directo al dispositivo.

---

# 46. Aplicaciones nativas

Una aplicación AriCodeOS no recibirá acceso arbitrario al sistema.

Manifest conceptual:

```toml
name = "photos"
version = "1.0"

[permissions]
filesystem = ["pictures:read"]
gpu = true
network = false
camera = false
microphone = false
```

Durante instalación o primer uso el sistema podrá solicitar autorización al usuario.

---

# 47. Runtime portable / WebAssembly

Una posibilidad importante para AriCodeOS es soportar aplicaciones WASM.

Arquitectura:

```text
WASM Application
      |
      v
AriCode Runtime
      |
      v
Capability API
      |
      v
System services
```

Beneficios:

- portabilidad;
- sandbox natural;
- menor dependencia de CPU;
- API controlada;
- distribución sencilla.

El soporte WASM no sustituirá necesariamente a aplicaciones nativas.

---

# 48. Compatibilidad Linux

No debe añadirse compatibilidad Linux dentro del microkernel principal.

Modelo futuro:

```text
AriCodeOS
   |
   +-- Native Apps
   +-- WASM Apps
   +-- Linux Compatibility Environment
            |
            +-- container / VM / personality server
```

Esto permite mantener limpio el diseño principal.

---

# 49. Compatibilidad Windows

No será un objetivo inicial.

En el futuro podría existir:

```text
Windows VM
```

o una capa de compatibilidad aislada.

No se pretende replicar Win32 dentro del kernel.

---

# 50. Virtualización

Fase avanzada.

Posible uso de:

- Intel VT-x;
- AMD-V;
- IOMMU;
- hardware virtualization.

AriCodeOS podría evolucionar hacia capacidad de ejecutar entornos de compatibilidad completos como VMs altamente integradas.

---

# 51. Seguridad

Seguridad será característica de arquitectura, no añadido final.

Principios:

1. mínimo código en ring 0;
2. memory safety;
3. capabilities;
4. aislamiento de procesos;
5. drivers fuera del kernel;
6. W^X;
7. NX;
8. ASLR futuro;
9. IOMMU;
10. system image firmada;
11. updates A/B;
12. sandbox de aplicaciones;
13. límites de recursos;
14. separación de secretos;
15. auditoría.

---

# 52. Protección frente a agotamiento de recursos

Cada proceso deberá poder disponer de cuotas.

```text
memory quota
CPU quota
IPC queue limits
handle limits
open object limits
shared memory limits
```

Una aplicación no debe poder colapsar el sistema simplemente creando millones de objetos.

---

# 53. Panic del kernel

Un kernel panic debe intentar preservar información diagnóstica.

Formato ejemplo:

```text
============================
ARICODEOS KERNEL PANIC
============================
Reason: page fault
CPU: 2
Thread: 41
Process: nvme.driver
RIP: ...
RSP: ...
CR2: ...
Backtrace:
  ...
============================
```

QEMU deberá poder enviar logs a puerto serie para facilitar tests automáticos.

---

# 54. Logging

Niveles:

```text
TRACE
DEBUG
INFO
WARN
ERROR
FATAL
```

Destinos iniciales:

```text
serial
framebuffer console
QEMU debug port opcional
```

Más adelante:

```text
structured logging service
```

---

# 55. Tests

Los tests son parte estructural del proyecto y viven en el repositorio.

Estructura implementada (ver `tests/README.md`):

```text
libs/*/src/lib.rs          #[cfg(test)]  unitarios de host (elf, x86-paging, cmdline)
boot-protocol/src/lib.rs   #[cfg(test)]  clasificación de memoria, iterador, cmdline
tools/xtask/src/*.rs       #[cfg(test)]  validez de los escenarios y utilidades
tests/qemu/*.toml                        escenarios de integración en QEMU
```

Cada fase añade su área (`boot`, `panic`, `memory`, `paging`, `interrupts`,
`scheduler`, `ipc`, `capabilities`, `drivers`, `filesystem`, `networking`,
`security`) como nuevos escenarios TOML y nuevos tests unitarios, en la misma
rama que el código.

---

# 56. Test runner en QEMU y niveles

```bash
cargo xtask test                 # unit + smoke      (por defecto; mínimo hacia develop)
cargo xtask test --level unit    # solo host, segundos
cargo xtask test --all           # unit + full       (obligatorio hacia main)
cargo xtask test --area boot     # solo los escenarios QEMU de un área
cargo xtask test --list          # ver escenarios
```

Salida:

```text
[xtask] nivel Full
[xtask] unit tests (host)
test result: ok. 18 passed
[PASS] boot.kernel_panic_reports              1.6s
[PASS] boot.kernel_ready                      1.6s
[PASS] boot.loader_missing_kernel             1.5s
[PASS] boot.loader_rejects_bad_elf            1.5s
[xtask] 4 escenarios QEMU, 0 fallidos
```

Un escenario declara cmdline, inyecciones de fallo en la ESP (kernel truncado,
kernel ausente) y expectativas sobre la serie y el código de salida de QEMU
(33 éxito, 35 fallo reportado). El comportamiento de test dentro del sistema se
pide por cmdline (`qemu_exit`, `test=panic`), nunca por features de
compilación (ADR 0007): un único binario sirve para todos los escenarios.

QEMU termina automáticamente con código de salida interpretable por CI.

---

# 57. Tipos de tests

## Unit tests

Para algoritmos independientes del hardware.

## Kernel integration tests

Arranque completo en QEMU.

## Fault injection tests

Provocar deliberadamente:

- page fault;
- invalid opcode;
- driver crash;
- IPC timeout;
- memoria insuficiente.

## Security tests

Comprobar que un proceso NO puede:

- leer memoria de otro;
- mapear kernel memory;
- utilizar dispositivos sin capability;
- falsificar handles;
- escalar privilegios mediante IPC.

## Hardware tests

Ejecutados sobre el portátil de pruebas.

---

# 58. CI

Pipeline futuro:

```text
commit
  |
  +-- cargo fmt
  +-- cargo clippy
  +-- host unit tests
  +-- build bootloader
  +-- build kernel
  +-- build image
  +-- QEMU boot test
  +-- kernel tests
  +-- integration tests
  +-- artifact image
```

Ningún commit que rompa el boot debería llegar a la rama principal.

---

# 59. Repositorio

Estructura propuesta:

```text
aricodeos/
│
├── README.md
├── LICENSE
├── Cargo.toml
├── rust-toolchain.toml
│
├── boot/
│   └── uefi-loader/
│
├── boot-protocol/
│
├── kernel/
│   ├── src/
│   │   ├── arch/
│   │   │   └── x86_64/
│   │   ├── memory/
│   │   ├── interrupt/
│   │   ├── scheduler/
│   │   ├── ipc/
│   │   ├── capability/
│   │   ├── object/
│   │   ├── syscall/
│   │   └── debug/
│   └── Cargo.toml
│
├── drivers/
│   ├── pci/
│   ├── nvme/
│   ├── virtio/
│   ├── usb/
│   ├── network/
│   └── gpu/
│
├── services/
│   ├── init/
│   ├── device-manager/
│   ├── filesystem/
│   ├── network/
│   ├── compositor/
│   └── audio/
│
├── runtime/
│   ├── native/
│   └── wasm/
│
├── sdk/
│   ├── rust/
│   └── abi/
│
├── apps/
│   ├── shell/
│   ├── terminal/
│   └── settings/
│
├── tests/
│   ├── kernel/
│   ├── integration/
│   └── hardware/
│
├── tools/
│   ├── xtask/
│   ├── image-builder/
│   └── debugger/
│
├── docs/
│   ├── architecture/
│   ├── abi/
│   ├── security/
│   ├── drivers/
│   └── decisions/
│
└── scripts/
```

`boot-protocol/` vive en la raíz del workspace y no dentro de `boot/`
porque es la única crate compartida por bootloader y kernel: define el
ABI de `BootInfo` y no debe depender de ninguno de los dos.

---

# 60. Arquitectura modular Rust

Workspace posible:

```text
aricodeos-boot-protocol
aricodeos-kernel
aricodeos-arch-x86_64
aricodeos-memory
aricodeos-ipc
aricodeos-capability
aricodeos-driver-api
aricodeos-sdk
aricodeos-test-framework
```

Debe evitarse crear cientos de crates sin necesidad.

La modularidad debe responder a límites arquitectónicos reales.

---

# 61. Convenciones de código

Principios:

- `unsafe` excepcional;
- documentar motivo de cada bloque `unsafe`;
- encapsular operaciones inseguras detrás de interfaces seguras;
- evitar estado global mutable;
- evitar allocations ocultas en rutas críticas;
- errores explícitos;
- no ignorar `Result`;
- no utilizar `unwrap()` en código crítico salvo invariantes demostrables;
- tests para cada bug corregido;
- documentar invariantes de concurrencia;
- medir antes de optimizar.

---

# 62. Política de unsafe

Cada bloque deberá explicar:

```text
SAFETY:
- precondición 1
- precondición 2
- por qué no existe aliasing inválido
- lifetime garantizado
- ownership del recurso
```

Objetivo:

```text
unsafe pequeño, localizado y auditable
```

---

# 63. ADR: Architecture Decision Records

Toda decisión importante deberá registrarse.

Ejemplo:

```text
docs/decisions/
0001-use-rust.md            (incluye la valoración frente a AriCode, sección 7.4)
0002-use-uefi-only.md
0003-microkernel.md
0004-capability-security.md
0005-user-space-drivers.md
```

Cada ADR incluirá:

- contexto;
- decisión;
- alternativas;
- consecuencias;
- fecha;
- estado.

---

# 64. Versionado

Primer esquema:

```text
0.0.1 boot
0.0.2 memory
0.0.3 interrupts
...
```

Antes de disponer de una API estable se utilizarán versiones `0.x`.

Una futura `1.0` debería significar como mínimo:

- boot estable;
- procesos;
- memoria;
- IPC;
- drivers esenciales;
- almacenamiento;
- red;
- SDK básico;
- actualización fiable;
- documentación del ABI.

---

# 65. Roadmap completo

---

## FASE 0 — Preparación del proyecto

### Objetivo

Tener un entorno reproducible de desarrollo.

### Trabajo

- crear repositorio Git;
- crear Rust workspace;
- fijar toolchain Rust;
- instalar QEMU;
- instalar OVMF;
- crear `cargo xtask` o equivalente;
- crear estructura básica de documentación;
- configurar `rustfmt`;
- configurar `clippy`;
- definir convenciones.

### Criterio de finalización

```text
cargo xtask build
```

produce correctamente la estructura inicial del proyecto.

---

# FASE 1 — UEFI Hello World

### Objetivo

Ejecutar código propio como aplicación EFI.

### Resultado esperado

```text
AriCodeOS UEFI loader
```

visible en pantalla.

### Criterio de finalización

QEMU + OVMF ejecuta `BOOTX64.EFI` generado por el proyecto.

---

# FASE 2 — Bootloader + kernel ELF

### Objetivo

Separar bootloader y kernel.

### Trabajo

- cargar kernel ELF desde partición EFI;
- parsear ELF64;
- reservar segmentos;
- construir BootInfo;
- ejecutar `ExitBootServices()`;
- saltar a entrypoint del kernel.

### Criterio

El kernel imprime:

```text
AriCodeOS Kernel 0.0.x
```

sin utilizar servicios UEFI después de entrar.

---

# FASE 3 — Consola temprana

**Estado: completada (2026-09-26).**

### Objetivo

Logging fiable.

### Implementado

- serial logger (COM1);
- framebuffer logger: `libs/fb-console` con fuente Terminus 8x16 (OFL 1.1),
  rejilla de celdas y scroll sin leer VRAM;
- macros: `print!/println!` (texto sin formato) y `log::{trace..error}!`
  con niveles TRACE..ERROR vía `console::KernelLogger`; `log=<nivel>` en la
  cmdline; FATAL equivale al panic;
- panic handler solo por serie.

### Criterio

Kernel panic legible y salida capturable por tests: escenarios
`boot.kernel_panic_reports` y área `console` (cuatro escenarios de niveles).

---

# FASE 4 — CPU foundation

**Estado: completada (2026-09-26).**

### Implementado

- GDT propia (código/datos ring 0, TSS) codificada en `libs/x86-cpu`;
- TSS con IST1 -> pila dedicada de 16 KiB para #DF;
- IDT de 256 vectores sobre stubs generados en ensamblador (ADR 0008);
- excepciones con volcado de registros, CR2/acceso/modo/causa para #PF,
  #BP recuperable;
- fallos provocados por cmdline: `test=breakpoint|invalid_opcode|page_fault|double_fault`.

### Tests (área `interrupts`)

- `interrupts.breakpoint_returns` (smoke): #BP manejado, el kernel llega a READY;
- `interrupts.invalid_opcode_panics`: #UD con volcado y pánico;
- `interrupts.page_fault_reports_cr2`: #PF con CR2, ACCESS WRITE, MODE KERNEL;
- `interrupts.double_fault_uses_ist`: pila corrupta + int3 -> #DF atendido en IST.

Además, 7 tests de host validan las codificaciones de descriptores, TSS e IDT
contra valores conocidos.

---

# FASE 5 — Physical Memory Manager

**Estado: completada (2026-09-26).**

### Implementado

- parser del mapa UEFI en `kernel/src/memory/pmm.rs`;
- bitmap allocator en `libs/pmm` (sin `alloc`, almacenamiento aportado por
  el llamador, `allocate`, `allocate_contiguous` alineado, `free` con
  detección de double free y fuera de rango);
- reservas: frame 0 y primer MiB, el propio bitmap;
- estadísticas (`Stats`: total, libres, usados).

### Tests

- host (7): liberación solo de frames completos, unicidad y orden, double
  free, redondeo de reservas, contigüidad con huecos, agotamiento;
- QEMU (área `memory`): `memory.pmm_init` (smoke) y `memory.pmm_selftest`
  (`test=pmm`, 2048 frames sobre el bitmap real).

---

# FASE 6 — Virtual Memory Manager

**Estado: completada (2026-09-26).**

### Implementado

- `libs/x86-paging` genérica en `FrameAlloc` y `PhysMap`: `map_4k`,
  `map_2m`, rangos, `unmap_4k`, `translate`, errores explícitos;
- `kernel/memory/vmm`: tablas propias con frames del PMM, imagen con W^X por
  secciones del linker, mapa físico lineal RW+NX, framebuffer, pila nueva
  de 64 KiB con guard page, cambio de pila a `kernel_main`, recuperación de
  la memoria de arranque;
- protocolo v3: AriBoot construye el mapa físico lineal desde el principio
  (ADR 0009), así que el cambio de tablas no invalida punteros;
- detección de desbordamiento de pila en #DF.

### Tests

- host (6 en `x86-paging`): traducción y enmascarado de flags, páginas
  grandes y colisiones, unmap, agotamiento, `OffsetMap`/`BumpFrames`;
- QEMU (área `memory`): `memory.vmm_init` (smoke), `memory.vmm_selftest`
  (lectura/escritura válidas, sin identidad, map/unmap),
  `memory.wx_rodata_write_faults` (write protection),
  `memory.nx_data_exec_faults` (NX fault), `memory.stack_guard_page`
  (guard page); `interrupts.page_fault_reports_cr2` cubre el unmapped fault.

---

# FASE 7 — Kernel allocator

**Estado: completada (2026-09-26).**

### Implementado

- `libs/heap`: first-fit con lista de libres ordenada, fusión de vecinos y
  cabecera por asignación (bump -> linked-list de la sección 16);
- `kernel/memory/heap`: `#[global_allocator]` con región virtual reservada
  de 64 MiB y crecimiento por trozos de 1 MiB con frames del PMM;
- `alloc` disponible en el kernel.

### Tests

- host (5): reutilización y fusión, alineaciones grandes sin fugas,
  agotamiento, 500 asignaciones en orden pseudoaleatorio sin solapes,
  regiones múltiples y colas absorbidas;
- QEMU: `memory.heap_init` (smoke) y `memory.heap_selftest` (`Vec`, `Box`,
  `String`, `format!`, crecimiento, `try_reserve` fallando limpio, sin fugas).

---

# FASE 8 — APIC y timer

**Estado: completada (2026-09-26).**

### Implementado

- `libs/sync`: `IrqLock` con política de interrupciones; sustituye a la
  celda de una sola CPU en todo el estado global del kernel;
- PIC 8259 remapeado y enmascarado; Local APIC habilitado (vector espurio
  0xFF) y mapeado sin caché;
- timer del LAPIC calibrado con el PIT y periódico a 100 Hz (vector 0x20);
- tick y reloj monótono (`time::{ticks, uptime_ms, sleep_ms}`);
- interrupciones externas habilitadas al final del arranque.

### Criterio

Interrupciones periódicas estables: `timer.init` (smoke) y `timer.selftest`
(5 ticks en 50 ms medidos por el PIT, `sleep_ms` exacto, cero espurias).
La ejecución prolongada se cubrirá con el scheduler (Fase 10).

---

# FASE 9 — Threads

**Estado: completada (2026-09-26).**

### Implementado

- kernel threads (`kernel/src/task`), con el contexto de arranque adoptado
  como hilo `main`;
- pilas de 32 KiB con guard page en región propia, devueltas al PMM al
  recoger el hilo;
- context switch System V en ensamblador (`switch_context`, `thread_start`);
- estados READY/RUNNING/BLOCKED/SLEEPING/TERMINATED en `libs/sched`
  (política probada en host: 6 tests);
- idle thread (`hlt` + `yield`).

### Criterio

Dos threads alternan ejecución correctamente: `threads.selftest` exige la
secuencia `ABABAB` con `yield` y que 40 workers terminen y sean recogidos sin
perder frames.

---

# FASE 10 — Scheduler

**Estado: completada (2026-09-26).**

### Primera versión

Round Robin preemptivo con quantum de 20 ms sobre el tick del APIC, sueño
planificado y colas de espera (`task::wait::WaitQueue`).

### Tests (`scheduler.selftest`, área `scheduler`)

- fairness básica: tres hilos ocupados sin `yield` reparten la CPU;
- yield: cubierto por `threads.selftest`;
- sleep: 50 ms medidos en ticks;
- bloqueo y wakeup: `wake_one` libera a un consumidor y `wake_all` al resto.

---

# FASE 11 — User mode

**Estado: completada (2026-09-26).**

### Objetivo

Ejecutar código en ring 3.

### Implementado

- user address spaces: PML4 por proceso con la mitad del kernel compartida,
  liberación completa al terminar (`libs/x86-paging::free_user_half`);
- transición kernel/user: `iretq` a ring 3, GDT con segmentos DPL 3, TSS.rsp0
  y CR3 por hilo en cada cambio de contexto;
- syscall entry: instrucción `syscall`/`sysretq`, ABI propio en `sdk/abi`
  (6 syscalls iniciales), validación de punteros de usuario;
- protección de páginas: USER solo en la mitad baja, código RX, pila RW+NX.

### Criterio crítico

Un proceso user no puede leer ni escribir memoria kernel:
`user.kernel_read_killed` y `user.kernel_write_killed` provocan #PF de
protección en ring 3, el proceso termina y el kernel llega a READY. Además
`user.privileged_killed` (#GP por `hlt`), `user.bad_pointer` (la syscall
rechaza un puntero de kernel) y `user.bad_syscall` (número inexistente).

---

# FASE 12 — Kernel objects + handles

**Estado: completada (2026-09-26), ADR 0010.**

### Implementado

- object manager: `kernel/src/object` con `KernelObject` (`Thread`,
  `Process`, `Console`) y tabla inicial por proceso;
- handle tables: `libs/handles` con índice + generación, límite por proceso
  (256), 4 tests de host;
- lifetime: `Arc` por objeto; cerrar un handle invalida ese valor para siempre;
- rights: `READ WRITE DUPLICATE TRANSFER WAIT SIGNAL MANAGE`; duplicar solo
  reduce; cada syscall declara el derecho que exige.

### Resultado

Las syscalls dejan de depender de identificadores globales inseguros:
`console_write` exige un handle de consola con `WRITE` (ABI v2), y
`user.handles` verifica desde ring 3 los casos de handle inventado, cerrado,
sin derechos, escalada y objeto equivocado.

---

# FASE 13 — Capabilities

**Estado: completada (2026-09-26).**

### Implementado

- capabilities: handles con derechos sobre objetos compartidos por `Arc`
  (Fase 12);
- rights: `READ WRITE DUPLICATE TRANSFER WAIT SIGNAL MANAGE`;
- transfer: `HandleTable::take` (exige `TRANSFER`), consumido por el IPC
  de la Fase 14;
- revoke/drop: `object_revoke` (exige `MANAGE`) invalida el objeto para
  todos sus handles en todos los procesos; `handle_close` descarta uno;
- derivation limitada: duplicar solo reduce derechos; un objeto revocado no
  se duplica.

Además, infraestructura adelantada de la Fase 27: `sdk/rust` (`aricode-sys`)
con los wrappers de syscall y `user/testprogs` con programas de ring 3 en
Rust y ensamblador enlazados como ELF estáticos, cargados por el kernel con
`aricode-elf`.

### Test esencial

Un proceso sin capability no puede acceder al recurso aunque conozca su
identificador: `user.handles` (los identificadores de `object_id` no dan
acceso; un handle inventado o cerrado falla) y `capabilities.revoke`
(revocar exige `MANAGE`; tras revocar, ningún duplicado da acceso).

---

# FASE 14 — IPC

**Estado: completada (2026-09-26), salvo memoria compartida.**

### Implementado

- endpoints: objeto del kernel con cola acotada y colas de espera;
- send / receive: `ipc_send` (`WRITE`) e `ipc_receive` (`READ`), mensajes de
  hasta 256 bytes y 4 capabilities;
- reply: por un endpoint de respuesta que el cliente transfiere en el mensaje;
- timeout: en ms, `0` no bloquea, infinito espera; despertar anticipado;
- capability transfer: `TRANSFER` obligatorio; se retira del emisor solo
  cuando el mensaje entra en la cola;
- shared memory: pendiente (llegará con `memory_map` y objetos de memoria,
  sección 24).

### Benchmark

`test=ipc` mide el round trip con el TSC: ~260 000 ciclos por round trip
bajo QEMU TCG (incluye crear el proceso cliente). Es una referencia relativa
hasta disponer de KVM o hardware (sección 69).

---

# FASE 15 — Init process

**Estado: completada (2026-09-26).**

### Primer proceso user-space

`services/init` (AriInit), cargado desde `aricodeos/initrd.img` (formato
propio en `libs/initrd`, leído por AriBoot y entregado en `BootInfo`).

Responsabilidades implementadas:

- iniciar servicios: `process_spawn` con la capacidad `SPAWNER`, que solo
  recibe init;
- supervisar procesos: `process_wait` distingue salida normal de muerte por
  fallo (`Killed`);
- gestionar dependencias básicas: pendiente hasta que existan servicios
  reales (Fase 16 en adelante).

Verificado por `init.boot` (smoke): lanza `hello`, ve morir `kernel_read`,
y comprueba que sin `MANAGE` no se pueden crear procesos.

---

# FASE 16 — Driver framework

**Estado: completada (2026-09-26).**

### Implementado

- driver API: syscalls `irq_wait`, `mmio_map`, `region_len`, `dma_alloc`,
  `dma_phys` en `sdk/abi` y `aricode-sys`;
- MMIO capability: objeto `Mmio` mapeado sin caché en el proceso;
- IRQ capability: objeto `Irq` sobre GSI del I/O APIC (ACPI MADT, `libs/acpi`),
  con enmascarado por nivel y liberación del vector;
- DMA buffer API: objeto `Dma` de frames contiguos a cero, `MANAGE` sobre
  `Hardware` para reservarlo;
- process restart: AriInit reinicia `crashy_driver` tras verlo morir.

### Criterio

Un driver puede fallar sin provocar panic del kernel: `init.boot` (driver
muerto y reiniciado) y `drivers.user_irq_mmio_dma` (IRQ, DMA y MMIO desde
ring 3).

---

# FASE 17 — PCIe enumeration

**Estado: completada (2026-09-26).**

### Implementado

- ACPI MCFG (`libs/acpi`);
- ECAM mapeado sin caché, 1 MiB por bus;
- enumerate: recorrido desde el bus inicial siguiendo puentes, funciones
  múltiples;
- BARs decodificados (E/S, memoria de 32/64 bits, prefetchable) y tamaño
  por sondeo en la librería;
- MSI inicial: detección de las capabilities MSI y MSI-X (la programación
  llega con virtio, Fase 18).

### Salida real en QEMU q35

```text
pci: 00:00.0 8086:29c0 clase 06.00 host bridge (0 BARs)
pci: 00:01.0 1234:1111 clase 03.00 VGA/display (2 BARs)
pci: 00:02.0 8086:10d3 clase 02.00 Ethernet (4 BARs, PM/MSI/PCIe/MSI-X)
pci: 00:1f.0 8086:2918 clase 06.01 ISA bridge (0 BARs)
pci: 00:1f.2 8086:2922 clase 01.06 SATA (AHCI) (2 BARs, MSI)
pci: 00:1f.3 8086:2930 clase 0c.05 SMBus (1 BAR)
```

---

# FASE 18 — Virtio

**Estado: completada (2026-09-26): virtio-blk (Fase 18) y virtio-net (Fase 22) en ring 3 sobre un transporte PCI moderno común (`aricode-sys::virtio_pci`).**

### Implementado

- virtio transport: `libs/virtio` (virtqueue split sobre DMA, descubrimiento
  de estructuras virtio-pci modernas, negociación por `common_cfg`);
- virtio-blk: `services/virtio-blk`, driver en ring 3 con capabilities
  reclamadas por `init` (`pci_claim`: BARs sondeados y MSI-X programada por
  el kernel), cola 0 en DMA, lectura y escritura verificadas
  (`virtio.blk_rw`);
- virtio-net: `services/virtio-net`, driver y pila de red en un mismo servicio (Fase 22).

### Motivo

Disponer rápidamente de almacenamiento y red en QEMU.

---

# FASE 19 — Storage service

**Estado: completada (2026-09-26), salvo la caché.**

### Implementado

- block service: el driver virtio-blk atiende `BlockServiceProtocol v1`
  (`sdk/proto`) por IPC con acceso directo al buffer DMA del cliente;
- async request queue: la cola acotada del endpoint;
- cache inicial: pendiente (llegará con el filesystem cuando haya métricas);
- partition parsing: GPT con CRC (`libs/gpt`), disco de pruebas con
  partición AriCodeOS (`ARI0`).

---

# FASE 20 — Filesystem básico

**Estado: completada (2026-09-26) con AriFS v0.**

### Implementado

- AriFS v0 (`libs/arifs`, no_std, tests de host): superbloque, tabla plana
  de 32 ficheros de capacidad fija (16 sectores), lectura y escritura por
  desplazamiento, sin fragmentación ni journaling (deliberadamente simple,
  sección 34.2). xtask formatea la partición `ARI0` del disco de pruebas
  con ficheros de ejemplo;
- servicio de ficheros `fsd` (`services/arifs`) en ring 3 sobre el servicio
  de bloques: `FileServiceProtocol v1` (`sdk/proto::fs`) con `List`,
  `Open`, `Create`, `Read`, `Write`, `Close`; los datos viajan por el buffer
  DMA del cliente, no por el mensaje;
- clientes en el SDK (`aricode-sys::block`, `aricode-sys::fs`) y programa
  de prueba `fs_client`; escenario `filesystem.read_write`;
- permisos mediante capabilities: un proceso solo habla con `fsd` si init
  le entrega un duplicado `WRITE` de su endpoint; `fsd` solo llega al disco
  a través del endpoint del servicio de bloques.

### Pendiente

- directorios jerárquicos y paths (v0 es un directorio plano);
- metadata (fechas, tipo) y borrado/redimensionado;
- capabilities por fichero (hoy la capability es por servicio).

### Criterio

Proceso user-space puede abrir, leer y escribir un archivo mediante servicio IPC. **Cumplido.**

---

# FASE 21 — Shell

**Estado: completada (2026-09-26).**

Primer shell AriCodeOS: `arish` (`services/shell`), un proceso de ring 3 que
init lanza cuando la cmdline lleva `shell` (`cargo xtask run --cmdline shell`).

Comandos:

```text
help      ps        mem       ls        cat NOM
devices   services  uptime    reboot    shutdown  exit
```

### Implementado

- entrada de consola en el kernel: IRQ de COM1 (GSI 4, vector propio) con
  anillo de 1 KiB y `console_read(console, buf, timeout)`; la consola sigue
  siendo del kernel y el shell solo la lee a través de un handle `CONSOLE`
  con `READ` que init le transfiere;
- objeto `SYSTEM` (kind 11): `system_info` (`READ`: uptime, memoria
  utilizable/libre, lista de procesos) y `system_power` (`MANAGE`: apagar,
  reiniciar). Init lo recibe y le pasa al shell un duplicado;
- el shell ve solo lo que init le da: endpoint de fsd (`WRITE`), hardware
  para su buffer DMA, `SYSTEM`, entrada de consola (`READ`) y la raíz PCI en
  solo lectura para `devices`; `services` enseña la máscara de servicios que
  init dejó activos y comprueba que fsd responde;
- `shutdown` termina QEMU con `qemu_exit` (`ARICODEOS_SHUTDOWN`) o detiene la
  CPU; `reboot` pulsa el reset del 8042 y, si no, fuerza un triple fallo;
- el runner de escenarios acepta `stdin` (líneas) y `prompt`: envía cada
  línea cuando la serie muestra el prompt. Escenarios `console.input`,
  `shell.commands`, `shell.exit` y `shell.reboot`.

---

# FASE 22 — Red

**Estado: completada (2026-09-26).**

### Implementado

- `libs/net` (no_std, sin `alloc`, tests de host): Ethernet II, ARP con
  caché, IPv4 (checksum, sin fragmentación), ICMP eco, UDP con checksum,
  cliente DHCP (DISCOVER/OFFER/REQUEST/ACK/NAK con reintentos) y DNS
  (consulta A, respuesta con compresión, y un constructor de respuestas para
  servidores de prueba);
- `netd` (`services/virtio-net`): driver virtio-net (colas rx/tx en DMA,
  MSI-X, `VIRTIO_NET_F_MAC`) y pila de red en un único hilo de ring 3: DHCP
  al arrancar, ARP con tramas aparcadas hasta resolver, respuesta a eco
  ICMP, sockets UDP y resolución DNS. Atiende `NetServiceProtocol v1`
  (`sdk/proto::net`): `Info`, `Ping`, `UdpBind`, `UdpSend`, `UdpRecv`,
  `UdpClose`, `Dns`; los datagramas van por el buffer DMA del cliente y las
  esperas (ping, recv, dns) quedan pendientes con plazo sin bloquear al
  servicio;
- contrato de driver ampliado: tras los BARs, init pasa extras propios de
  cada driver (netd recibe `SYSTEM` con `READ` para medir el tiempo);
- transporte virtio compartido (`aricode-sys::virtio_pci`): virtio-blk y
  virtio-net ya no duplican la negociación ni la creación de colas;
- cliente `aricode-sys::net`, programa `net_client`, comandos `net`,
  `ping` y `dns` en el shell;
- QEMU con red de usuario (slirp) y un respondedor UDP de pruebas en el
  host (eco y DNS con registro A fijo) que arranca con cada ejecución.

### Criterio

`ping` funciona dentro de QEMU (`ping 10.0.2.2` desde el shell responde 4 de
4; `network.stack` lo exige por IPC). **Cumplido.**

---

# FASE 23 — TCP

**Estado: completada (2026-09-26).**

TCP vive en `netd` (servicio de usuario), con la máquina de estados en
`libs/net::tcp` y sin `alloc`: conexión con buffers fijos de 4 KiB,
apertura activa y pasiva, MSS 1400, retransmisión por temporizador
exponencial (500 ms → 8 s, 6 reintentos), cierre ordenado con TIME-WAIT y
RST. `NetServiceProtocol v1` añade `TcpConnect`, `TcpListen`, `TcpAccept`,
`TcpSend`, `TcpRecv` y `TcpClose`; las esperas quedan pendientes con plazo.

Tests (todos en `libs/net`, sobre un cable simulado con pérdidas
controladas, más `network.tcp` en QEMU contra un servidor de eco del host
y una sonda que se conecta al invitado por `hostfwd`):

- handshake: `handshake_data_and_close`;
- retransmisión: `retransmits_lost_syn_and_data`, `gives_up_after_too_many_retries`;
- cierre: `handshake_data_and_close` (FIN en ambos sentidos, TIME-WAIT), `reset_closes_immediately`;
- pérdida simulada: `random_loss_keeps_stream_in_order` (25 % de pérdidas, 10 000 bytes en orden);
- múltiples conexiones: `multiple_independent_connections` y dos conexiones simultáneas en `network.tcp`.

Pendiente: ventana deslizante con varios segmentos en vuelo retransmitidos
individualmente (hoy se reenvía desde `snd_una`), opciones de escala de
ventana y SACK, y estimación de RTT.

---

# FASE 24 — HTTP client

**Estado: completada (2026-09-26).**

Primera aplicación de red real: `libs/http` (peticiones `GET`, análisis de
la línea de estado y cabeceras, cuerpos por `Content-Length`, `chunked` o
hasta el cierre; sin `alloc`, tests de host), `aricode-sys::http::get`
sobre el cliente TCP de netd (resuelve nombres por DNS si el host no es una
IP), el programa `http_client` y el comando `http URL` del shell. xtask
sirve `/hola`, `/chunked` y 404 en `127.0.0.1:17010` para `network.http`.

Objetivo `http GET -> respuesta recibida`: **cumplido** (200 con cuerpo,
chunked, 404 y puerto cerrado detectado).

TLS vendrá posteriormente (Fase 31, criptografía).

---

# FASE 25 — Input moderno

**Estado: virtio-input completado (2026-09-27); xHCI/USB HID pendiente.**

### Implementado

- `libs/input` (no_std, tests de host): eventos en formato evdev (el mismo
  que usa virtio-input y que servirá para USB HID) y un teclado US con
  mayúsculas y bloqueo de mayúsculas que convierte pulsaciones en
  caracteres;
- `inputd` (`services/virtio-input`): driver virtio-input en ring 3 sobre el
  transporte virtio común, un proceso por dispositivo (init lanza uno por
  cada función `1af4:1052`); lee el nombre del dispositivo de su
  configuración, mantiene la cola de eventos y atiende
  `InputServiceProtocol v1` (`sdk/proto::input`: `Info`, `Read` con plazo);
- cliente `aricode-sys::input`, programa `input_client` y escenario
  `input.virtio`: xtask inyecta por QMP (`input-send-event`) la línea
  `hola` + Enter en el teclado virtio y un movimiento y clic en el ratón
  virtio; el escenario exige la traducción exacta.

### Pendiente

- xHCI, USB y HID (teclado y ratón físicos): misma librería de eventos,
  driver nuevo en ring 3;
- que el shell pueda leer del teclado virtio además de la consola serie
  (llegará con la GUI, Fase 26, donde el teclado y el ratón alimentan al
  compositor).

---

# FASE 26 — GUI básica

**Estado: completada (2026-09-27).**

### Implementado

- framebuffer abstraction: con `gui` en la cmdline el kernel no pinta en la
  pantalla y entrega a init el framebuffer GOP como capability MMIO
  (`INIT_FRAMEBUFFER`); `system_info(FRAMEBUFFER)` describe geometría y
  formato. La consola de kernel queda en la serie;
- surfaces: `libs/gfx` (no_std, sin alloc, tests de host): lienzos
  `0x00RRGGBB` sobre buffers del llamador con recorte, rectángulos, texto
  con la fuente de la consola, cursor y el modelo `Desktop` (ventanas en
  cascada, orden z, foco, prueba de impacto, composición);
- compositor (`services/compositor`): proceso de ring 3 dueño del
  framebuffer; compone en un buffer trasero DMA y lo vuelca convirtiendo el
  formato; lee los servicios de entrada, ordena sus eventos por instante de
  llegada (inputd los sella) y enruta teclado y clics a la ventana con foco;
- mouse pointer: flecha dibujada por el compositor, movida con los eventos
  relativos del ratón virtio;
- windows: `WindowProtocol v1` (`sdk/proto::window`): `Create` (el cliente
  aporta el buffer de contenido como DMA), `Present`, `Poll`, `Close`;
  cliente `aricode-sys::window::Window` con lienzo propio;
- events: `Key` (carácter traducido), `Click` (coordenadas del contenido,
  botón) y `Focus`, con espera pendiente por ventana.

### Criterio

Dos aplicaciones gráficas aisladas pueden dibujar simultáneamente:
**cumplido**. `gui.two_windows` lanza `gui_demo A` y `gui_demo B` (procesos
distintos con sus propios buffers), comprueba los píxeles de cada ventana en
la composición, hace clic sobre B por QMP y teclea `h`: solo B recibe foco,
clic y tecla; A nunca ve eventos ajenos.

### Pendiente

- mover y redimensionar ventanas, cerrar desde la barra de título;
- composición por regiones dañadas (hoy se repinta la pantalla completa);
- que el shell y la consola vivan en una ventana (terminal gráfico).

---

# FASE 27 — Application SDK

**Estado: completada (2026-09-27).** Ver `docs/sdk.md`.

SDK inicial Rust, en `sdk/`: `aricode-abi`, `aricode-sys` (syscalls y
clientes de servicios), `aricode-proto` (protocolos), `aricode-rt` (runtime:
`entry!`, heap, panic, formato) y `aricode-build` (enlace). El runtime dejó
de vivir en los programas de prueba: servicios, aplicaciones y tests
dependen de las mismas crates. `apps/hello` es la aplicación de ejemplo y el
shell puede lanzar programas del initrd con `run` (recibe un `SPAWNER`).

APIs y dónde están:

```text
process      sys::process_spawn_with / process_wait / process_exit
ipc          sys::endpoint_create / ipc_send / ipc_receive
filesystem   sys::fs::FsClient
network      sys::net::NetClient, sys::http::get
window       sys::window::Window (lienzo aricode-gfx)
input        sys::input::InputClient, aricode-input::Keyboard
time         sys::time::{uptime_ms, Clock, sleep_ms}
permissions  sys::permissions::{granted, kind_name, is, drop_handle}
```

---

# FASE 28 — App sandbox

**Estado: completada (2026-09-27).**

### Implementado

- manifests: `libs/manifest` (no_std, tests de host) analiza
  `NOMBRE.manifest` (`name`, `caps`, `memory`); xtask lo empaqueta en el
  initrd junto al programa y el kernel lo sirve a init con `initrd_read`
  (`READ` sobre el `SPAWNER`);
- capability broker: init atiende `LaunchProtocol v1` (`sdk/proto::launch`)
  mientras vive el shell; el shell ya no tiene `SPAWNER`, solo un endpoint
  al broker. Por cada lanzamiento el broker lee el manifiesto, deniega las
  capacidades reservadas al sistema (`spawner`, `pci`, `framebuffer`) y
  entrega en orden fijo solo lo pedido: `SYSTEM` de lectura, directorio
  privado, red, compositor, entradas y, si hace falta, `HARDWARE`. Sin
  manifiesto: consola y reloj;
- permisos de usuario: la política vive en el broker (`Caps::APP_ALLOWED`);
  `sys::permissions` deja a la aplicación ver lo que recibió;
- directorio privado por app: `fs::Op::Bind` crea en fsd un endpoint ligado
  a un prefijo (`app/`); desde él solo se listan, abren y crean ficheros
  propios (AriFS admite `/` en los nombres). El mismo prefijo comparte
  endpoint entre lanzamientos;
- límites de recursos: cuota de memoria DMA por proceso, fijada por quien lo
  crea (`spawn_limits` en `process_spawn`) y cobrada en `dma_alloc`
  (`Error::Limit` al excederla).

`hello_app` (manifiesto `system, fs, 128K`) demuestra las cuatro cosas en
`sandbox.manifest`: ve solo su fichero `notas`, no puede abrir `hola.txt` y
una reserva de 256 KiB falla con `Limit`; un programa sin manifiesto recibe
solo consola y reloj.

### Pendiente

- cuota de CPU y de handles por manifiesto; cuota de memoria de heap (hoy el
  heap es estático);
- identidad de cliente en IPC (que fsd distinga procesos sin un endpoint por
  cada uno);
- un fichero de política editable por el usuario en AriFS en vez de la
  constante del broker.

---

# FASE 29 — WASM runtime

**Estado: completada (2026-09-27).**

### Objetivo

Ejecutar una aplicación portable y aislada. **Cumplido**: `apps/wasm-demo`
se compila con `rustc` a `wasm32-unknown-unknown` (MVP + `sign-ext` +
`bulk-memory`) y `wasm_runner`, una aplicación con manifiesto (`system, fs,
512K`), lo carga de su directorio privado y lo ejecuta con `libs/wasm`, un
intérprete propio `no_std` (control estructurado, locales y globales,
memoria lineal con límites comprobados, llamadas directas e indirectas,
importaciones; sin coma flotante ni SIMD, con tope de pasos y de
profundidad). La memoria lineal del módulo es un buffer DMA del runner,
acotado por la cuota del manifiesto. Escenario `wasm.demo`.

### API

No se expone POSIX: el módulo solo importa la Capability API bajo el módulo
`aricode` (`print(ptr, len)`, `uptime_ms() -> i64`), resuelta por nombre
por el anfitrión, que valida cada acceso a memoria. Todo lo que el anfitrión
no importa no existe para el módulo.

### Pendiente

- más funciones de la Capability API (ficheros, red y ventanas del propio
  runner) y un formato de manifiesto para módulos WASM;
- coma flotante y validación completa del módulo antes de ejecutar (hoy se
  confía en el productor y se comprueba en tiempo de ejecución);
- rendimiento: es un intérprete byte a byte sin precompilación.

---

# FASE 30 — Actualización A/B

**Estado: completada (2026-09-27).**

### Implementado

- system image: contenedor `aricode-initrd` con `manifest`, `manifest.sig`,
  `kernel.elf` (sin depuración) e `initrd.img`, precedido en la partición
  por una cabecera `ARISLOT` con su longitud; xtask lo construye y firma
  con la clave de desarrollo (`image::system_image`);
- manifest firmado: `version`, SHA-256 de kernel e initrd; firma Ed25519
  del texto completo; `aricode-ab::Image::verify` solo devuelve los bytes
  si firma y hashes cuadran;
- slot A/B: particiones GPT `SYSA` y `SYSB` (12 MiB), `UPDATE` (imagen
  descargada) y `ARIBOOT` (estado); AriBoot las localiza por `BlockIo`
  (apertura no exclusiva), elige el slot, verifica y carga el kernel de la
  imagen; si no hay `ARIBOOT` o nada verifica, arranca desde la ESP
  (recuperación). El kernel recibe `boot_slot=A|B [boot_pending]` por la
  cmdline e init lo ve en `init_arg`;
- boot attempts counter: `BootState` (sector 0 de `ARIBOOT`, con CRC)
  guarda activo, pendiente e intentos; AriBoot consume un intento antes de
  saltar al kernel e init confirma el slot escribiendo el estado por el
  servicio de bloques cuando los servicios de arranque funcionan;
- rollback: pendiente sin intentos o con imagen rechazada → vuelve al
  activo y borra el pendiente (`aricode-ab::{choose, reject}`).

El actualizador (`apps/updater`, aplicación de confianza con la capacidad
`block`) lee `UPDATE`, verifica, copia al slot inactivo y programa tres
intentos. Escenarios: `update.slot_a` (smoke), `update.pending_boot`,
`update.rollback_exhausted`, `update.reject_bad_signature`,
`update.esp_fallback` y `update.apply`, cada uno con su variante de disco.

### Pendiente

- reinicio automático tras instalar y prueba de dos arranques encadenados
  (hoy cada escenario parte de un disco preparado por xtask);
- origen de la actualización por red (hoy la deja xtask en `UPDATE`);
- clave de producción fuera del repositorio y rotación de claves.

---

# FASE 31 — Criptografía y confianza

**Estado: completada (2026-09-27); la firma de imágenes se aplica en la Fase 30.**

### Implementado

- `libs/crypto` (no_std, sin dependencias, tests con los vectores oficiales):
  SHA-256 y SHA-512 (FIPS 180-4), Ed25519 (RFC 8032: campo GF(2^255-19) en
  limbs de 51 bits, puntos extendidos, escalares módulo L) y ChaCha20
  (RFC 7539). Ed25519 no es de tiempo constante: el sistema solo verifica;
  firmar es cosa del host;
- firma de imágenes: `cargo xtask sign` y `cargo xtask verify` sobre
  ficheros; los manifiestos de actualización firmados llegan con la Fase 30;
- hash verification: SHA-256 incremental para imágenes grandes;
- key management: `cargo xtask keygen`; par de desarrollo versionado en
  `keys/dev` (solo pruebas, ver su README) y regla de que la semilla de
  producción nunca entra en el repositorio;
- secure random: generador ChaCha20 en el kernel, servido por
  `system_info(RANDOM)` (`sys::random`);
- entropy sources: `rdrand` si la CPU lo tiene, y muestras del TSC y del
  tiempo de arranque en cada semilla y en cada lectura; el kernel anuncia
  qué fuente usa (`rdrand` o `TSC (sin rdrand: entropia limitada)`).

### Pendiente

- `rdseed` y fuentes externas (jitter de interrupciones, virtio-rng);
- Ed25519 de tiempo constante si algún día el sistema firma.

---

# FASE 32 — SMP

Aunque parte de SMP puede adelantarse, en esta etapa deberá consolidarse:

- AP discovery;
- startup;
- per-CPU state;
- scheduler SMP;
- TLB shootdown;
- locks;
- CPU affinity.

---

# FASE 33 — IOMMU

### Implementar

- VT-d o AMD equivalent;
- device domains;
- DMA mappings;
- isolation.

---

# FASE 34 — Hardware real

Aquí entra el portátil viejo.

No hace falta formatearlo durante las primeras fases.

Primero se utilizará QEMU.

Cuando AriCodeOS sea capaz de:

- arrancar UEFI;
- mostrar logs;
- gestionar memoria;
- manejar excepciones;

se crea un USB bootable.

## Política inicial

**No tocar el disco interno.**

Arrancar AriCodeOS desde USB.

Solo cuando el almacenamiento sea estable se planteará dedicar el portátil completamente al sistema.

---

# 66. Portátil de pruebas

Información que deberá recopilarse antes de trabajar con hardware real:

```text
CPU exacta
RAM
GPU
chipset
controlador NVMe/SATA
WiFi
Ethernet
USB controller
UEFI version
Secure Boot state
ACPI tables
PCI device list
```

Desde Linux pueden guardarse como documentación del hardware:

```bash
lscpu
lspci -nn
lsusb
sudo dmidecode
lsblk
sudo dmesg
```

Así tendremos una especificación de nuestro primer hardware físico objetivo.

---

# 67. Estrategia de hardware

No intentar soportar todo PC existente.

Primero definir un **AriCodeOS Reference Machine**.

Ejemplo:

```text
Reference Platform 1
--------------------
UEFI x86-64
ACPI
APIC
PCIe
NVMe
USB xHCI
GOP framebuffer
Intel/AMD CPU
```

Si el portátil elegido cumple los requisitos, se convierte en nuestra primera plataforma real certificada.

---

# 68. Estrategia de compatibilidad

Una regla arquitectónica importante:

> La compatibilidad antigua debe vivir encima de AriCodeOS, nunca dentro de su núcleo.

```text
                 AriCodeOS
                    |
        +-----------+-----------+
        |           |           |
      Native       WASM       Legacy
       Apps         Apps      Environment
                                |
                              Linux
```

Así podemos conservar un kernel moderno incluso cuando necesitemos ejecutar software antiguo.

---

# 69. Rendimiento

La arquitectura microkernel puede introducir IPC adicional.

Por tanto, AriCodeOS no debe asumir que microkernel significa automáticamente más rápido.

Se medirá:

```text
syscall latency
IPC latency
context switch latency
page fault latency
scheduler overhead
interrupt latency
storage throughput
network throughput
memory bandwidth
boot time
power consumption
```

Las optimizaciones se harán con métricas reales.

---

# 70. Benchmarks internos

Repositorio futuro:

```text
bench/
├── syscall
├── ipc
├── context-switch
├── allocator
├── memory-map
├── filesystem
├── network
└── scheduler
```

Cada cambio crítico podrá compararse frente a un baseline.

---

# 71. Observabilidad

AriCodeOS debe diseñarse para poder explicar qué está haciendo.

Herramientas futuras:

```text
process inspector
thread inspector
memory map viewer
IPC tracer
capability inspector
device inspector
scheduler trace
performance counters
```

Esto será extremadamente importante durante el desarrollo.

---

# 72. Debugging

Primeras herramientas:

- serial logs;
- QEMU monitor;
- GDB remote debugging;
- symbols ELF;
- panic backtraces.

Configuración conceptual:

```text
QEMU --gdb tcp::1234 --stop
        |
        v
GDB
        |
        v
kernel.elf symbols
```

---

# 73. Crash dumps

Fase posterior:

Cuando exista almacenamiento fiable, el sistema podrá guardar crash dumps.

Contenido:

- kernel version;
- CPU state;
- process;
- thread;
- stack;
- registers;
- fault;
- last log buffer.

---

# 74. API y ABI

Deben distinguirse claramente:

```text
Kernel internal API
Kernel syscall ABI
Service IPC protocols
Application API
Driver ABI
Boot protocol
```

No deben evolucionar todos al mismo ritmo.

Durante `0.x` pueden romper compatibilidad.

Posteriormente se versionarán.

---

# 75. Protocolos IPC versionados

Ejemplo:

```text
FileServiceProtocol v1
NetworkServiceProtocol v1
WindowServiceProtocol v1
```

Cada mensaje deberá incluir identificador/versionado cuando sea necesario.

---

# 76. Boot protocol versionado

Ejemplo conceptual:

```rust
const BOOT_PROTOCOL_VERSION: u32 = 1;
```

El kernel debe comprobar que entiende la versión entregada por el bootloader.

---

# 77. Nombres internos sugeridos

```text
AriKernel      -> kernel
AriBoot        -> bootloader
AriInit        -> primer proceso
AriIPC         -> IPC
AriCap         -> capabilities
AriFS          -> filesystem service
AriNet         -> network service
AriGUI         -> compositor / GUI
AriRuntime     -> runtime de aplicaciones
AriSDK         -> SDK
AriPkg         -> futuro package manager
```

No son obligatorios; sirven como convención inicial.

---

# 78. Futuro package manager

No será una prioridad inicial.

Modelo previsto:

```text
package
├── signed manifest
├── executable / wasm
├── resources
├── requested capabilities
└── version metadata
```

Instalación transaccional.

Las apps no modificarán el sistema base.

---

# 79. Configuración

Evitar un registro global monolítico.

Preferencia:

```text
system configuration service
user configuration
per-app configuration
```

Formato humano cuando sea útil y representación binaria/versionada para protocolos internos.

---

# 80. Usuarios

No es una prioridad del kernel inicial.

Cuando se implemente identidad humana, deberá separarse de capabilities.

```text
User identity != authority automática
```

La identidad puede participar en políticas, pero las capabilities determinan el acceso real a objetos.

---

# 81. Seguridad de aplicaciones

Toda aplicación deberá considerarse potencialmente comprometible.

Objetivo:

```text
Browser comprometido
       |
       X kernel
       X otras apps
       X raw disk
       X microphone si no tiene permiso
       X arbitrary user files
```

El sandbox debe impedir que un fallo de una aplicación se transforme automáticamente en compromiso total del sistema.

---

# 82. Seguridad de drivers

Un driver se tratará igualmente como código potencialmente defectuoso.

Ejemplo:

```text
WiFi Driver crash
      |
      +-- se registra fallo
      +-- device manager revoca capabilities
      +-- proceso termina
      +-- driver reinicia
      +-- conexiones se recuperan cuando sea posible
```

No debe provocar corrupción de memoria kernel.

---

# 83. Principios de diseño que NO deben romperse

1. Kernel pequeño.
2. Seguridad por defecto.
3. Drivers aislados.
4. Compatibilidad heredada fuera del kernel.
5. APIs explícitas.
6. No introducir legacy sin justificación.
7. Test antes de integrar.
8. Cada privilegio debe poder explicarse.
9. Cada `unsafe` debe poder justificarse.
10. Cada optimización importante debe poder medirse.

---

# 84. Política frente a legacy

Antes de añadir una característica heredada preguntar:

```text
¿la necesitamos realmente?
¿qué hardware actual la utiliza?
¿puede implementarse fuera del kernel?
¿puede ser una capa opcional?
¿incrementa el TCB?
¿abre una nueva superficie de ataque?
```

Por defecto:

```text
NO LEGACY
```

hasta demostrar necesidad.

---

# 85. Primer objetivo práctico

## AriCodeOS 0.0.1

Debe hacer solamente esto:

```text
UEFI
  |
  v
AriBoot
  |
  v
load kernel.elf
  |
  v
ExitBootServices
  |
  v
AriKernel
  |
  v
framebuffer / serial
  |
  v
"AriCodeOS booting..."
```

Nada más.

Ese milestone es importante porque demostrará que existe una cadena completamente propia desde firmware hasta kernel.

---

# 86. AriCodeOS 0.0.2

**Estado: completada (2026-09-26), tag `v0.0.2`.**

Añadido:

- BootInfo v2 (con cmdline);
- memory map clasificado;
- logging con niveles (serie + framebuffer);
- panic handler y excepciones con diagnóstico;
- GDT/TSS;
- IDT con IST para double fault.

Incluye además la infraestructura de tests por niveles y las dos rondas de
auditoría (`docs/audits/`).

---

# 87. AriCodeOS 0.0.3

**Estado: completada (2026-09-26), tag `v0.0.3`.**

Añadido:

- physical memory allocator (bitmap, `libs/pmm`);
- virtual memory (tablas propias, W^X, mapa físico lineal, guard page;
  protocolo de arranque v3, ADR 0009);
- kernel heap (`libs/heap`, crecimiento bajo demanda).

---

# 88. AriCodeOS 0.0.4

**Estado: completada (2026-09-26), tag `v0.0.4`.**

Añadido:

- `IrqLock` en todo el estado global (`libs/sync`);
- APIC (PIC enmascarado, LAPIC, timer calibrado con PIT, 100 Hz);
- threads (pilas con guard, context switch, idle);
- scheduler round robin preemptivo, sueño y colas de espera (`libs/sched`).

---

# 89. AriCodeOS 0.0.5

**Estado: completada (2026-09-26), tag `v0.0.5`.**

Añadido:

- ring 3 (GDT con segmentos DPL 3, `iretq`, fallos de usuario no fatales);
- syscalls (`syscall`/`sysretq`, ABI v2 en `sdk/abi`);
- procesos (hilo principal, salida y muerte, liberación del espacio);
- address spaces (PML4 por proceso con mitad del kernel compartida);
- objetos y handles con derechos (Fase 12 adelantada, ADR 0010).

---

# 90. AriCodeOS 0.0.6

**Estado: completada (2026-09-26), tag `v0.0.6`.**

Añadido:

- kernel objects y handles (Fase 12, en 0.0.5);
- capabilities con revocación y transferencia (Fase 13);
- IPC por endpoints con timeouts y transferencia de capabilities (Fase 14);
- SDK mínimo (`sdk/rust`) y programas de usuario en Rust (`user/testprogs`).

Esta versión marca el nacimiento real de la arquitectura microkernel: dos
procesos en ring 3 se comunican por un endpoint sin que ninguno vea al otro
más que a través de las capabilities que se le han concedido.

---

# 91. AriCodeOS 0.0.7

**Estado: completada (2026-09-26), tag `v0.0.7`.**

Añadido:

- init process (AriInit desde el initrd, capacidad SPAWNER, supervisión);
- driver framework (capabilities IRQ, MMIO y DMA; drivers de prueba en ring 3);
- device manager: la enumeración y el reparto de capabilities siguen en el
  kernel (banco de pruebas); el servicio de usuario llega con virtio;
- PCIe (ECAM por MCFG, enumeración, BARs, capabilities).

---

# 92. AriCodeOS 0.0.8

**Estado: completada (2026-09-26), tag `v0.0.8`.**

Añadido:

- virtio-blk (Fase 18): driver en ring 3 con capabilities PCI, MSI-X y DMA;
- storage service (Fase 19): `BlockServiceProtocol v1` sobre disco GPT;
- filesystem (Fase 20): AriFS v0 y el servicio `fsd` por IPC;
- shell (Fase 21): `arish`, entrada de consola por IRQ, `system_info` y
  `system_power`.

Con esta versión un usuario puede teclear en la consola serie, listar y leer
ficheros de un disco virtio a través de dos servicios en ring 3 y apagar la
máquina sin que ningún driver viva en el kernel.

---

# 93. AriCodeOS 0.0.9

**Estado: completada (2026-09-26), tag `v0.0.9`.**

Añadido:

- virtio-net (Fase 22): driver en ring 3 sobre el transporte virtio común;
- IP (Fase 22): IPv4, ARP, ICMP, DHCP, DNS;
- UDP (Fase 22);
- TCP básico (Fase 23): conexiones activas y pasivas con retransmisión y
  cierre ordenado, todo en `netd`.

Con esta versión el sistema obtiene dirección por DHCP, responde y hace
ping, intercambia UDP y TCP con el host y resuelve nombres, sin que el
kernel conozca ningún protocolo de red.

---

# 94. AriCodeOS 0.1

**Estado: completada (2026-09-26), tag `v0.1.0`; hotfix `v0.1.1`
(2026-09-27) con las correcciones de la auditoría HTTP.** Fases 0 a 24: arranque
UEFI, memoria, procesos en ring 3, scheduler, IPC y capabilities, drivers
virtio en espacio de usuario, servicio de bloques, AriFS, shell, pila de
red con TCP y cliente HTTP.

Primera versión experimental utilizable.

Objetivo aproximado:

```text
boot
memory
processes
scheduler
IPC
capabilities
filesystem
shell
network
basic drivers
```

Todavía sin ser un sistema operativo de escritorio completo.

## 94.1 AriCodeOS 0.2

**Estado: completada (2026-09-27), tag `v0.2.0`.** Fases 25 a 27:

- entrada moderna: virtio-input (teclado y ratón) con eventos sellados en el
  tiempo y traducción de teclado en el cliente;
- GUI básica: compositor en ring 3 dueño del framebuffer, ventanas con
  buffer propio, cursor, foco y enrutado de entrada; dos aplicaciones
  aisladas dibujan a la vez;
- SDK de aplicaciones: runtime común (`aricode-rt`), `aricode-build`, APIs
  de proceso, IPC, ficheros, red, ventanas, entrada, tiempo y permisos,
  documentadas en `docs/sdk.md`; `apps/hello` y `run` en el shell.

## 94.2 AriCodeOS 0.3

**Estado: completada (2026-09-27), tag `v0.3.0`; hotfix `v0.3.1` con las
correcciones de la auditoría del SDK.** Fases 28 y 29:

- sandbox de aplicaciones: manifiestos, broker de capabilities en init,
  directorio privado por aplicación en fsd y cuota de memoria DMA por
  proceso; el shell lanza a través del broker y ya no puede crear procesos;
- runtime WebAssembly: intérprete propio `no_std` y `wasm_runner`, una
  aplicación con manifiesto que ejecuta módulos portables compilados con
  `rustc` exponiendo solo la Capability API.

## 94.3 AriCodeOS 0.4

**Estado: completada (2026-09-27), tag `v0.4.0`; hotfix `v0.4.1` con las
correcciones de la auditoría e9fdf38.** Fases 30 y 31:

- criptografía propia (SHA-256, SHA-512, Ed25519, ChaCha20), generador
  aleatorio del kernel y herramientas de claves;
- actualización A/B: imágenes de sistema firmadas en particiones de slot,
  estado de arranque con intentos y rollback en AriBoot, confirmación por
  init y actualizador con capacidad de bloques.

Siguiente: 0.5 con SMP (Fase 32) e IOMMU (Fase 33).

---

# 95. Hardware real: momento correcto

No es necesario formatear el portátil antiguo inicialmente.

Orden recomendado:

```text
1. QEMU
2. QEMU + UEFI
3. QEMU integration tests
4. boot USB en portátil
5. probar RAM / ACPI / framebuffer
6. probar PCIe
7. probar almacenamiento en modo read-only
8. escribir sobre USB externo
9. solo mucho después considerar disco interno
```

Esto minimiza el riesgo de destruir datos o quedar bloqueado durante desarrollo.

---

# 96. Estrategia de disco del portátil

Inicialmente:

```text
Internal SSD/HDD: NO TOCAR
USB AriCodeOS:     BOOT
```

Más adelante:

```text
USB SSD dedicado a AriCodeOS
```

Finalmente, si el proyecto madura:

```text
portátil completamente dedicado
```

---

# 97. Seguridad del entorno de pruebas

Antes de probar drivers de almacenamiento reales:

- hacer backup del portátil;
- desconectar discos innecesarios si es posible;
- utilizar un USB dedicado;
- implementar modo read-only;
- confirmar IDs de dispositivos;
- añadir protecciones contra escritura accidental.

---

# 98. Definition of Done por fase

Cada fase se considerará terminada únicamente si cumple:

```text
[ ] implementación
[ ] documentación
[ ] tests
[ ] sin warnings injustificados
[ ] boot test automático
[ ] código revisado
[ ] arquitectura actualizada
[ ] ADR si introduce decisión relevante
[ ] commit/tag reproducible
```

---

# 99. Gestión del proyecto

Modelo adoptado (git flow), detallado en `docs/WORKFLOW.md`:

```text
main        -> siempre arrancable; solo merges etiquetados desde develop u hotfix/*
develop     -> integración; debe pasar cargo xtask test
feature/*   -> desarrollo, parte y vuelve a develop
fix/*       -> correcciones sobre develop
hotfix/*    -> correcciones urgentes sobre main (vuelven también a develop)
experiment/*-> prototipos que pueden romperse
```

Ningún commit directo en `main` ni `develop`. Merges siempre con `--no-ff` y
con `cargo xtask check` y `cargo xtask test` en verde. El primer commit
(`v0.0.1`, M0) es la única excepción histórica.

---

# 100. Backlog de investigación

Temas a estudiar conforme avance el proyecto:

- UEFI specification;
- x86-64 System Programming;
- ELF64;
- ACPI;
- APIC;
- PCI Express;
- NVMe;
- xHCI;
- virtio;
- IOMMU;
- WASM;
- GPU APIs;
- SMP scheduling;
- lock-free data structures;
- cryptographic boot;
- formally specified IPC/capabilities.

---

# 101. Riesgos técnicos

## Drivers

Es probablemente uno de los trabajos más grandes de cualquier SO.

Mitigación:

```text
QEMU + virtio primero
hardware objetivo limitado después
```

## GPU

Los drivers GPU modernos son extremadamente complejos.

Mitigación inicial:

```text
UEFI framebuffer
virtio-gpu
```

## WiFi

Hardware y firmware propietarios pueden complicar soporte.

Mitigación:

- Ethernet primero;
- adaptadores/documentación conocida;
- dispositivos USB soportables posteriormente.

## Compatibilidad

No intentar resolverla durante el nacimiento del kernel.

## Scope creep

Evitar construir navegador, escritorio, paquete, IA, etc. antes de estabilizar fundamentos.

---

# 102. Métrica de éxito del proyecto

El éxito temprano NO será:

> ejecutar Chrome.

El éxito temprano será:

```text
AriCodeOS arranca
AriCodeOS controla su memoria
AriCodeOS recupera fallos
AriCodeOS ejecuta procesos aislados
AriCodeOS puede negar accesos
AriCodeOS reinicia drivers
AriCodeOS puede testearse automáticamente
```

---

# 103. Principio de independencia

El objetivo a largo plazo es llegar al punto donde AriCodeOS pueda desarrollar AriCodeOS.

Evolución:

```text
Debian (estación de desarrollo)
  |
  v
compila AriCodeOS

AriCodeOS
  |
  v
runtime + toolchain

AriCodeOS
  |
  v
compila nueva versión de AriCodeOS
```

Ese momento representará un sistema **self-hosting**.

No es un objetivo de las primeras fases.

---

# 104. Self-hosting futuro

Para llegar a ello será necesario disponer de:

- filesystem estable;
- terminal;
- editor;
- procesos;
- memoria madura;
- compilador/toolchain;
- linker;
- build system;
- git o alternativa;
- almacenamiento fiable.

Podría comenzar con cross-compilación y posteriormente portar LLVM/Rust u otra toolchain.

---

# 105. Relación con AriCode

El nombre AriCodeOS encaja naturalmente con el compilador AriCode existente,
pero tras la valoración de la sección 7.4 la relación queda definida así:

```text
AriCode NO participa en el kernel, el bootloader ni el TCB.
AriCode es una carga de trabajo de usuario y, más adelante, un runtime.
```

## 105.1 Camino corto: ejecutar binarios AriCode sin tocar el compilador

Los binarios que produce `aric` son ELF estáticos que solo usan un conjunto
pequeño de syscalls Linux:

```text
read  write  exit  mmap  open  close
socket  bind  listen  accept  epoll_*
clone  futex  wait4
```

Un *personality server* de compatibilidad Linux (sección 48), ejecutado en
espacio de usuario sobre AriKernel, puede implementar ese subconjunto y
cargar ELF estáticos en `0x400000`. Eso permitiría ejecutar los modelos
`aricode_es100m` y el resto de programas AriCode sobre AriCodeOS sin
modificar el compilador. Es la primera integración prevista y encaja con la
regla de la sección 68: lo antiguo se ejecuta encima, nunca define el núcleo.

## 105.2 Camino largo: target nativo AriCodeOS

Cuando el ABI de syscalls (sección 22) y el SDK (Fase 27) sean estables,
podrá añadirse a `aric` un target `x86_64-aricodeos` que emita el formato
de aplicación nativo y use la Capability API en lugar de syscalls Linux.
Esto es trabajo del compilador AriCode, no del kernel, y no bloquea ninguna
fase del roadmap.

## 105.3 Lo que AriCode aporta al proyecto

- Carga de trabajo real y exigente (inferencia int8/int4 multihilo, SHA-256,
  secp256k1) para validar scheduler, memoria y la capa de compatibilidad.
- Un caso de uso concreto para el diseño de scheduling heterogéneo
  (sección 19) y para futuros aceleradores.
- Una razón de peso para que la capa de compatibilidad Linux se diseñe
  desde el principio con ELF estático y sin libc como primer objetivo.

## 105.4 Requisitos que AriCode impone al roadmap

Ninguno antes de la Fase 15. A partir de ahí:

```text
Fase 11  user mode          -> necesario para cargar ELF estático
Fase 14  IPC                -> necesario para el personality server
Fase 20  filesystem         -> open/read de los .ari compilados y pesos
Fase 22  red                -> socket/epoll usados por aricode-net-http
Fase 32  SMP                -> clone/futex de aricode-ml multihilo
```

El kernel no dependerá de AriCode en ninguna fase.

---

# 106. Posible filosofía AriCodeOS

Una formulación posible:

> Hardware moderno, privilegios mínimos, componentes aislados y ninguna compatibilidad heredada sin una razón demostrable.

Otra:

> Lo antiguo puede ejecutarse encima. Nunca debe definir el núcleo.

---

# 107. Próximos pasos inmediatos

Orden recomendado para comenzar realmente:

```text
01. Crear repo aricodeos
02. Crear workspace Rust
03. Crear AriBoot UEFI
04. Ejecutarlo en QEMU/OVMF
05. Crear kernel ELF no_std
06. Cargarlo desde AriBoot
07. ExitBootServices
08. Saltar al kernel
09. Logger serial
10. Framebuffer console
11. Panic handler
12. Primer test automático de boot
```

No implementar todavía:

```text
network
filesystem
USB
GPU
WASM
desktop
```

hasta que el núcleo básico esté estabilizado.

---

# 108. Primer árbol de trabajo real

Para comenzar, el repositorio puede reducirse a:

```text
aricodeos/
├── Cargo.toml
├── rust-toolchain.toml
├── boot/
│   ├── Cargo.toml
│   └── src/main.rs
├── kernel/
│   ├── Cargo.toml
│   └── src/main.rs
├── boot-protocol/
│   ├── Cargo.toml
│   └── src/lib.rs
├── tools/
│   └── xtask/
└── docs/
    └── architecture.md
```

No necesitamos crear toda la estructura futura desde el primer día.

Este árbol es el que existe en el repositorio desde M0, ampliado con
`libs/{elf,x86-paging,x86-cpu,pmm,heap,sync,sched,handles,cmdline,fb-console}` y `sdk/abi` (lógica `no_std` con tests de host),
`tests/qemu/` (escenarios) y `docs/decisions/0001..0007`.

---

# 109. Primer test de aceptación

```text
TEST: boot_kernel_001

Given:
  QEMU x86-64
  OVMF UEFI

When:
  AriCodeOS image boots

Then:
  serial output contains:
  "ARICODEOS_KERNEL_READY"

And:
  QEMU exits with success status in test mode
```

Este test debe convertirse en la piedra angular del CI.

---

# 110. Resultado final de la arquitectura propuesta

```text
                         APPLICATIONS
                              |
             +----------------+----------------+
             |                |                |
           Native            WASM          Legacy VM
             |                |                |
             +----------------+----------------+
                              |
                        AriCode SDK
                              |
                    Capability API
                              |
      +-----------------------+-----------------------+
      |                       |                       |
     AriFS                   AriNet                  AriGUI
      |                       |                       |
      +-----------------------+-----------------------+
                              |
                       User Drivers
                              |
                   +----------+----------+
                   |          |          |
                  NVMe       USB        GPU
                   |          |          |
                   +----------+----------+
                              |
                       ARICODE KERNEL
              +---------------+----------------+
              |               |                |
           Scheduler        Memory             IPC
              |               |                |
           Threads        Page Tables      Capabilities
              |               |                |
              +---------------+----------------+
                              |
                           Hardware
```

---

# 111. Conclusión

AriCodeOS debe comenzar pequeño.

No necesitamos un portátil formateado, una interfaz gráfica ni cientos de drivers para decir que existe un sistema operativo propio.

La primera victoria será mucho más simple:

```text
UEFI
  -> AriBoot
  -> AriKernel
  -> control total de CPU y memoria
  -> "AriCodeOS booting..."
```

A partir de ahí cada nueva capacidad debe añadirse de forma controlada, aislada, documentada y testeada.

La arquitectura propuesta evita deliberadamente convertir AriCodeOS en una copia de los sistemas actuales. Toma estándares modernos de hardware donde son necesarios, pero mantiene como principios centrales:

```text
microkernel
memory safety
capabilities
user-space drivers
immutable system
atomic updates
modern hardware only
legacy outside the kernel
automated testing
```

El portátil antiguo debe reservarse como **plataforma física de validación**, no como entorno principal de desarrollo. QEMU será la plataforma de desarrollo inicial porque permite depuración, automatización, snapshots y fallos controlados sin poner hardware o datos en riesgo.

El objetivo a largo plazo es llegar a un sistema capaz de ejecutar aplicaciones modernas, aislar código potencialmente hostil, reiniciar componentes defectuosos y eventualmente compilar nuevas versiones de sí mismo.

---

# 112. Próximo milestone oficial

## AriCodeOS Milestone M0 — First Boot

**Objetivo:**

Construir una imagen UEFI arrancable que cargue un kernel ELF propio y produzca:

```text
AriCodeOS booting...
ARICODEOS_KERNEL_READY
```

por puerto serie y/o framebuffer.

**Entregables (estado 2026-09-26):**

```text
[x] repositorio                (git inicializado; primer commit pendiente)
[x] workspace Rust             Cargo.toml, rust-toolchain.toml 1.98.1
[x] AriBoot                    boot/  (crate ariboot, x86_64-unknown-uefi)
[x] AriKernel                  kernel/ (crate arikernel, x86_64-unknown-none)
[x] BootInfo v1                boot-protocol/
[x] image builder              cargo xtask image  -> build/esp/
[x] QEMU runner                cargo xtask run
[x] serial logger              COM1 16550, macros print!/println!
[x] framebuffer output         GOP, fondo + franjas de color
[x] panic handler              serie + exit code 35 en modo test
[x] boot integration test      cargo xtask test  -> [PASS] boot.kernel_ready
[x] README de compilación      README.md
```

Resultado del primer arranque en QEMU + OVMF (serie):

```text
AriCodeOS booting...
AriCodeOS Kernel 0.0.1
framebuffer: 1280x800 stride 1280 Bgr @ 0x80000000
kernel: phys 0x1e7bc000 virt 0xffffffff80000000 size 16 KiB
memory map: 117 regiones (desc 48 bytes)
ARICODEOS_KERNEL_READY
[PASS] boot.kernel_ready  (qemu_exit=Some(33), 1.5s)
```

**M0 completado.** AriCodeOS ha dejado de ser únicamente un diseño: existe una cadena propia firmware -> AriBoot -> AriKernel ejecutándose sobre una máquina x86-64 virtual.

Deuda técnica registrada en `docs/architecture.md`: identidad física RWX hasta el VMM (Fase 6), GDT/IDT del firmware hasta la Fase 4, sin consola de texto en framebuffer hasta la Fase 3.

**Siguiente milestone:** 0.0.2 (sección 86): logging con niveles, GDT, IDT, excepciones y double fault stack.

