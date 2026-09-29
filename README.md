# AriCodeOS

Sistema operativo desde cero: microkernel en Rust `no_std`, arranque UEFI,
x86-64, seguridad por capabilities y drivers en espacio de usuario.

El documento maestro de arquitectura y roadmap es [AriCodeOS.md](AriCodeOS.md).
Las decisiones de diseño se registran en [docs/decisions/](docs/decisions/) y el
flujo de ramas (git flow) en [docs/WORKFLOW.md](docs/WORKFLOW.md).

**Estado:** 0.6.0 (2026-09-28). Las 35 fases del roadmap (0 a 34) están
implementadas y probadas en QEMU; la Fase 34 espera su primera prueba en el
portátil (ver [docs/hardware.md](docs/hardware.md)). La tabla siguiente es
la referencia única del estado de cada capacidad: `validado en QEMU` significa
que hay escenarios en `tests/qemu/` que lo exigen en cada suite; nada está
todavía `validado en hardware`.

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
| WASM (intérprete MVP acotado) | prototipo, validado en QEMU | `wasm.*`; sin validación previa ni WASI |
| Criptografía propia, RNG, imágenes firmadas, A/B con rollback | validado en QEMU | `crypto.*`, `update.*`; no es Secure Boot |
| IOMMU VT-d con dominio por driver | validado en QEMU | `iommu.*` |
| Imagen USB para pendrive | validado en QEMU (`usb.image_boot`) | pendiente en hardware |
| Aplicaciones fuera del árbol: paquetes firmados instalados en AriFS (`cargo xtask app-pack`) | validado en QEMU | `apps.*`; instalación desde el propio sistema pendiente |
| Arranque en portátil (Fase 34) | propuesto | necesita el equipo de referencia |

## Requisitos (Debian 13)

```bash
sudo apt install qemu-system-x86 ovmf nasm clang lld gdb git make
rustup target add wasm32-unknown-unknown   # modulos WebAssembly de ejemplo (apps/wasm-demo)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

`rust-toolchain.toml` fija la versión de Rust y añade automáticamente los
targets `x86_64-unknown-none` y `x86_64-unknown-uefi`.

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
apps/updater/    actualizador A/B: verifica la imagen de UPDATE y la instala en el slot inactivo
libs/ab/         estado de arranque A/B, imagenes firmadas y politica de slots (AriBoot, init, updater)
boot/            AriBoot: cargador UEFI (crate `ariboot`)
kernel/          AriKernel (crate `arikernel`), ELF64 en la mitad alta
tools/xtask/     automatización: build, imagen, QEMU, tests, lint
tests/qemu/      escenarios de integración declarativos (TOML)
docs/            arquitectura, flujo de trabajo y ADRs
build/           salida generada (ESP, OVMF_VARS, disk.img, logs); no versionar
```

## Test de aceptación

`tests/qemu/boot_kernel_ready.toml` arranca QEMU sin pantalla con
`isa-debug-exit`, captura la serie y exige que aparezca
`ARICODEOS_KERNEL_READY`, que no haya `KERNEL PANIC` y que QEMU termine con
código 33. Los escenarios `full` inyectan fallos (ELF truncado, kernel
ausente, panic provocado) y exigen el código 35. Los logs quedan en
`build/scenarios/<nombre>/serial.log`.
