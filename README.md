# AriCodeOS

Sistema operativo desde cero: microkernel en Rust `no_std`, arranque UEFI,
x86-64, seguridad por capabilities y drivers en espacio de usuario.

El documento maestro de arquitectura y roadmap es [AriCodeOS.md](AriCodeOS.md).
Las decisiones de diseño se registran en [docs/decisions/](docs/decisions/) y el
flujo de ramas (git flow) en [docs/WORKFLOW.md](docs/WORKFLOW.md).

**Estado:** 0.1.0, primera versión experimental utilizable. AriBoot (UEFI)
carga `kernel.elf` e `initrd.img` y salta a AriKernel, que levanta consola
serie y de framebuffer con logger por niveles, instala GDT/TSS/IDT propias,
construye el allocator físico, activa sus propias tablas con W^X, arranca el
heap, calibra el timer del LAPIC a 100 Hz, planifica hilos con round robin
preemptivo y ejecuta procesos en ring 3 con espacio propio, syscalls, tablas
de handles con derechos y revocación e IPC por endpoints con transferencia de
capabilities. `init` lanza desde el initrd los servicios de ring 3: el driver
virtio-blk como servicio de bloques, `fsd` (AriFS v0), `netd` (virtio-net con
DHCP, ARP, ICMP, UDP, TCP y DNS) y, si la cmdline lleva `shell`, el shell
`arish` con `ls`, `cat`, `ps`, `mem`, `ping`, `dns`, `http`, `reboot` y
`shutdown`. Un proceso que toca memoria del kernel o ejecuta instrucciones
privilegiadas muere sin afectar al kernel, y `init` lo reinicia si es un
servicio. Los fallos se provocan desde la cmdline para los tests.

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
cargo xtask test           # unit + smoke (arranque nominal en QEMU)
cargo xtask test --all     # unit + todos los escenarios QEMU; obligatorio antes de main
cargo xtask test --area boot   # solo los escenarios de un área
cargo xtask check          # rustfmt --check + clippy -D warnings
```

Añade `--debug` a cualquier comando para usar el perfil de desarrollo. Los
niveles de test están descritos en [tests/README.md](tests/README.md).

Los programas de usuario y `init` van en `aricodeos/initrd.img`, que `cargo
xtask image` construye a partir de `user/testprogs` y `services/init`.

## Estructura

```text
boot-protocol/   ABI BootInfo compartido por bootloader y kernel
libs/            librerías no_std con tests de host: elf, x86-paging, x86-cpu, pmm, heap, sync, sched, handles, cmdline, fb-console
sdk/abi/         ABI de syscalls compartido entre kernel y espacio de usuario
sdk/rust/        aricode-sys: wrappers de syscall y linker script de los programas de usuario
user/testprogs/  programas de prueba de ring 3 y drivers de prueba (Rust y ensamblador), empaquetados en el initrd
services/init/   AriInit, primer proceso de usuario y device manager provisional
services/virtio-blk/ driver virtio-blk y servicio de bloques por IPC en ring 3
services/arifs/  fsd: servicio de ficheros AriFS v0 por IPC en ring 3
services/virtio-net/ netd: driver virtio-net y pila de red (DHCP, ping, UDP, TCP, DNS) por IPC en ring 3
services/virtio-input/ inputd: driver virtio-input (teclado, raton) con eventos por IPC en ring 3
services/compositor/ compositor: ventanas, cursor y enrutado de entrada sobre el framebuffer, en ring 3
libs/gfx/        graficos 2D sin alloc (lienzos, texto, cursor, escritorio) con tests de host
libs/input/      eventos evdev y teclado US con tests de host
libs/net/        pila de red mínima sin alloc con tests de host
libs/http/       cliente HTTP GET mínimo (Content-Length, chunked) con tests de host
services/shell/  arish: shell interactivo en ring 3
libs/arifs/      AriFS v0: sistema de archivos plano con tests de host
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
