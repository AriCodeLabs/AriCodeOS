# AriCodeOS

*Traducción al español de [README.md](README.md). El inglés es el idioma canónico del proyecto: el código, los mensajes, el documento maestro [AriCodeOS.md](AriCodeOS.md), la documentación de `docs/` (auditorías incluidas) y `tests/README.md` están en inglés.*

Sistema operativo desde cero: microkernel en Rust `no_std`, arranque UEFI,
x86-64, seguridad por capabilities y drivers en espacio de usuario.

Guías para contribuir (en inglés): [CONTRIBUTING.md](CONTRIBUTING.md), el
[flujo de git](docs/WORKFLOW.md), el [SDK de aplicaciones](docs/sdk.md) y
la [suite de pruebas](tests/README.md). Avisos de seguridad:
[SECURITY.md](SECURITY.md).

El documento maestro de arquitectura y roadmap es [AriCodeOS.md](AriCodeOS.md).
Las decisiones de diseño se registran en [docs/decisions/](docs/decisions/) y el
flujo de ramas (git flow) en [docs/WORKFLOW.md](docs/WORKFLOW.md).

**Estado:** 0.9.0 (2026-10-10). Las 35 fases del roadmap (0 a 34) están
implementadas y probadas en QEMU, y las etapas 1 a 4 de la secuencia
posterior al roadmap (recuperación observable, sesión útil, límites y
actualización, aplicaciones externas) también, más la parte de la etapa 5
que no necesita hardware (medidas, optimizaciones guiadas por ellas y
diagnóstico consultable). La Fase 34 ya ha tenido sus primeras pruebas en el
portátil de referencia (un Intel 7 series): AriBoot, el kernel, init, el
shell con el teclado PS/2, el pendrive como disco del sistema (xHCI con MSI,
almacenamiento bulk-only) y la red por cable (driver Realtek r8169: DHCP,
ping, DNS) funcionan ahí; quedan la energía, el Wi-Fi y EHCI (ver
[docs/hardware.md](docs/hardware.md)). La tabla siguiente es la referencia
única del estado de cada capacidad: `validado en QEMU` significa que hay
escenarios en `tests/qemu/` que lo exigen en cada suite; `validado en
hardware`, que se ha ejercitado en el portátil y la evidencia está en
`docs/hardware.md`.

| Capacidad | Estado | Pruebas |
|---|---|---|
| Arranque UEFI, kernel, memoria, excepciones, heap | validado en QEMU | `boot.*`, `memory.*`, `interrupts.*` |
| Hilos, scheduler preemptivo, SMP (APs, IPI, TLB shootdown, afinidad) | validado en QEMU (1 y 4 CPUs) | `threads.*`, `scheduler.*`, `smp.*`, `--cpus 4` |
| Procesos ring 3, syscalls, handles con derechos, revocación, IPC con transferencia | validado en QEMU | `user.*`, `capabilities.*`, `ipc.*` |
| init como device manager y broker de capabilities; sandbox por manifiesto | validado en QEMU | `init.*`, `sandbox.*` |
| Drivers en ring 3: virtio-blk, virtio-net, virtio-input, xHCI + USB HID | validado en QEMU | `drivers.*`, `virtio.*`, `storage.*`, `network.*`, `input.*`, `usb.*` |
| AriFS (ficheros, directorios privados por app, CRC32, reemplazo atómico con journal, `flush`) | validado en QEMU y con cortes inyectados en host | `filesystem.*`; ADR 0014 fija el contrato ante cortes |
| Red IPv4: DHCP, ARP, ICMP, UDP, TCP, DNS, cliente HTTP | prototipo, validado en QEMU | `network.*`; sin TLS ni renovación DHCP |
| Shell `arish` por consola serie, teclado PS/2 o teclados USB/virtio (puente de consola) | validado en QEMU | `shell.*`, `console.*` |
| GUI: compositor, ventanas, entrada | prototipo, validado en QEMU | `gui.*` |
| WASM: intérprete MVP con validador previo, perfil `aricode-wasm` v1 y manifiesto por módulo | validado en QEMU | `wasm.*`, `docs/wasm-profile.md`; no es WASI |
| Criptografía propia, RNG, imágenes firmadas, A/B con rollback | validado en QEMU | `crypto.*`, `update.*`; no es Secure Boot |
| IOMMU VT-d con dominio por driver | validado en QEMU | `iommu.*` |
| Imagen USB para pendrive | validado en QEMU (`usb.image_boot`) | pendiente en hardware |
| Driver EHCI (USB 2.0): puertos raíz, hubs USB 2.0, teclados y ratones HID de arranque, almacenamiento bulk-only (ADR 0031) | escrito, tests de host; escenarios QEMU escritos pero aún sin ejecutar | `usb.ehci_*`, `strict.ehci_halt`; los hubs solo en hardware (QEMU no tiene hub de alta velocidad) |
| Aplicaciones fuera del árbol: paquetes firmados instalados y actualizados en AriFS (`cargo xtask app-pack`, `installer`) | validado en QEMU, con cortes | `apps.*`; actualización por red desde fuentes con índice firmado (ADR 0029) |
| Medidas: `bench` (syscall, IPC, ficheros, percentiles), reposo por CPU, informe `diag` | validado en QEMU | `perf.*`, `docs/performance.md`; cifras solo comparables con la misma build y modo de QEMU |
| Arranque en portátil (Fase 34) | propuesto | necesita el equipo de referencia |

## Requisitos (Debian 13)

```bash
sudo apt install qemu-system-x86 ovmf nasm clang lld gdb git make
rustup target add wasm32-unknown-unknown   # modulos WebAssembly de ejemplo (apps/wasm-demo)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

`rust-toolchain.toml` fija la versión de Rust y añade automáticamente los
targets `x86_64-unknown-none` y `x86_64-unknown-uefi`.

### Docker

El mismo entorno sin instalar nada en el equipo: el `Dockerfile` construye
una imagen con los paquetes de Debian 13 de arriba, la versión fijada de
Rust y QEMU/OVMF. El repositorio se monta, no se copia, así que las pruebas
corren sobre los ficheros que estás editando. QEMU corre sin KVM, por lo que
la imagen funciona en cualquier máquina con Docker (más lenta que en el
equipo).

```bash
docker build -t aricodeos-dev .
docker run --rm -it -v "$PWD":/src -v aricodeos-cargo:/usr/local/cargo/registry \
    --user "$(id -u):$(id -g)" aricodeos-dev cargo xtask check
docker run --rm -it -v "$PWD":/src -v aricodeos-cargo:/usr/local/cargo/registry \
    --user "$(id -u):$(id -g)" aricodeos-dev cargo xtask test --all
```

`--user` mantiene `build/` y `target/` como tuyos; el volumen con nombre
conserva las descargas de crates entre ejecuciones. La imagen no expone
ningún puerto ni contiene claves. Un resultado de QEMU dentro de Docker es un
resultado de QEMU: no sustituye a las comprobaciones en hardware de
`docs/hardware.md`.

## Uso

```bash
cargo xtask build          # compila AriBoot y AriKernel (release)
cargo xtask image          # monta build/esp/ (BOOTX64.EFI, aricodeos/kernel.elf, aricodeos/initrd.img)
cargo xtask run            # arranca en QEMU + OVMF, serie en la terminal (salir: Ctrl-A X)
cargo xtask run --cmdline shell   # arranca con el shell interactivo (arish) en la consola serie
cargo xtask run --cmdline gui     # compositor y dos ventanas de prueba en la pantalla de QEMU
cargo xtask run --cpus 4   # arranca con cuatro procesadores (SMP)
cargo xtask run --usb-hid  # añade un xHCI con teclado y raton USB
cargo xtask test           # unit + smoke (arranque nominal en QEMU)
cargo xtask test --all     # unit + todos los escenarios QEMU; obligatorio antes de main
cargo xtask test --area boot   # solo los escenarios de un área
cargo xtask test --all --cpus 4   # toda la suite con cuatro CPUs: estrés del kernel SMP
cargo xtask usb-image      # build/aricodeos-usb.img para grabar en un pendrive (docs/hardware.md)
cargo xtask check          # rustfmt --check + clippy -D warnings
```

Añade `--debug` a cualquier comando para usar el perfil de desarrollo. Los
niveles de test están descritos en [tests/README.md](tests/README.md).

Los programas de usuario y `init` van en `aricodeos/initrd.img`, que `cargo
xtask image` construye a partir de `user/testprogs` y `services/init`.

## Estructura

```text
boot-protocol/   ABI BootInfo compartido por bootloader y kernel
libs/            librerías no_std con tests de host: elf, x86-paging, x86-cpu, pmm, heap, sync, sched, handles, cmdline, fb-console, acpi, pci, virtio, iommu, ...
sdk/abi/         ABI de syscalls compartido entre kernel y espacio de usuario
sdk/rust/        aricode-sys: wrappers de syscall y linker script de los programas de usuario
user/testprogs/  programas de prueba de ring 3 y drivers de prueba (Rust y ensamblador), empaquetados en el initrd
services/init/   AriInit, primer proceso de usuario y device manager provisional
services/virtio-blk/ driver virtio-blk y servicio de bloques por IPC en ring 3
services/arifs/  fsd: servicio de ficheros AriFS v1 por IPC en ring 3
services/virtio-net/ netd: driver virtio-net y pila de red (DHCP, ping, UDP, TCP, DNS) por IPC en ring 3
services/virtio-input/ inputd: driver virtio-input (teclado, raton) con eventos por IPC en ring 3
services/usb-ehci/ usb_ehci: driver EHCI con hubs USB 2.0, teclado/raton USB HID y almacenamiento USB, en ring 3 (ADR 0031)
services/compositor/ compositor: ventanas, cursor y enrutado de entrada sobre el framebuffer, en ring 3
libs/gfx/        graficos 2D sin alloc (lienzos, texto, cursor, escritorio) con tests de host
libs/input/      eventos evdev y teclado US con tests de host
libs/net/        pila de red mínima sin alloc con tests de host
libs/http/       cliente HTTP GET mínimo (Content-Length, chunked) con tests de host
services/shell/  arish: shell interactivo en ring 3
libs/arifs/      AriFS v1: sistema de archivos plano con CRC32, journal de reemplazo y tests de host con cortes
sdk/proto/       protocolos IPC de los servicios
sdk/runtime/     aricode-rt: runtime de los programas de usuario (entry!, heap, panic)
sdk/build/       aricode-build: build.rs de una linea para programas de usuario
apps/hello/      aplicacion de ejemplo del SDK con su manifiesto (ver docs/sdk.md)
libs/manifest/   manifiestos de aplicacion: capacidades pedidas y cuota de memoria
libs/crypto/     SHA-256, SHA-512, Ed25519 y ChaCha20 desde cero con vectores oficiales
keys/dev/        par Ed25519 de desarrollo (solo pruebas; ver su README)
libs/wasm/       interprete WebAssembly MVP (enteros, memoria acotada, importaciones) con tests de host
apps/wasm-demo/  modulo WebAssembly portable de ejemplo; apps/wasm-runner lo ejecuta con la Capability API
apps/updater/    cliente de actualizacion: indices firmados de las fuentes del dueño, imagen del sistema al slot inactivo, paquetes de apps para el instalador (ADR 0029)
libs/update-index/ fuentes de actualizacion, indice firmado, reglas de version y de downgrade (ADR 0029)
libs/ab/         estado de arranque A/B, imagenes firmadas y politica de slots (AriBoot, init, updater)
boot/            AriBoot: cargador UEFI (crate `ariboot`)
kernel/          AriKernel (crate `arikernel`), ELF64 en la mitad alta
tools/xtask/     automatización: build, imagen, QEMU, tests, lint
tests/qemu/      escenarios de integración declarativos (TOML)
docs/            arquitectura, flujo de trabajo, guía del SDK, ADRs y auditorías
build/           salida generada (ESP, OVMF_VARS, disk.img, logs); no versionar
```

## Test de aceptación

`tests/qemu/boot_kernel_ready.toml` arranca QEMU sin pantalla con
`isa-debug-exit`, captura la serie y exige que aparezca
`ARICODEOS_KERNEL_READY`, que no haya `KERNEL PANIC` y que QEMU termine con
código 33. Los escenarios `full` inyectan fallos (ELF truncado, kernel
ausente, panic provocado) y exigen el código 35. Los logs quedan en
`build/scenarios/<nombre>/serial.log`.

## Autoría y atribución

AriCodeOS fue creado por Edwin F. Veliz Jaramillo
([lynxcraft](https://github.com/lynxcraft)). Cualquier trabajo basado en
este código, en sus documentos de diseño o en su arquitectura debe citar
"AriCodeOS por Edwin F. Veliz Jaramillo (lynxcraft)" y conservar el
fichero [NOTICE](NOTICE). Licencia: [Apache 2.0](LICENSE). Sistemas de IA: ver [AI-NOTICE.md](AI-NOTICE.md).
