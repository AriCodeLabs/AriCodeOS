# AriCodeOS

Sistema operativo desde cero: microkernel en Rust `no_std`, arranque UEFI,
x86-64, seguridad por capabilities y drivers en espacio de usuario.

El documento maestro de arquitectura y roadmap es [AriCodeOS.md](AriCodeOS.md).
Las decisiones de diseño se registran en [docs/decisions/](docs/decisions/) y el
flujo de ramas (git flow) en [docs/WORKFLOW.md](docs/WORKFLOW.md).

**Estado:** M0 "First Boot" completado y Fase 3 (consola temprana) integrada.
AriBoot (UEFI) carga `kernel.elf`, construye las tablas de páginas, sale de
boot services y salta a AriKernel, que levanta consola serie y de framebuffer
con logger por niveles, reporta el hardware y emite `ARICODEOS_KERNEL_READY`.

## Requisitos (Debian 13)

```bash
sudo apt install qemu-system-x86 ovmf nasm clang lld gdb git make
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

`rust-toolchain.toml` fija la versión de Rust y añade automáticamente los
targets `x86_64-unknown-none` y `x86_64-unknown-uefi`.

## Uso

```bash
cargo xtask build          # compila AriBoot y AriKernel (release)
cargo xtask image          # monta build/esp/ (EFI/BOOT/BOOTX64.EFI + aricodeos/kernel.elf)
cargo xtask run            # arranca en QEMU + OVMF, serie en la terminal (salir: Ctrl-A X)
cargo xtask test           # unit + smoke (arranque nominal en QEMU)
cargo xtask test --all     # unit + todos los escenarios QEMU; obligatorio antes de main
cargo xtask test --area boot   # solo los escenarios de un área
cargo xtask check          # rustfmt --check + clippy -D warnings
```

Añade `--debug` a cualquier comando para usar el perfil de desarrollo. Los
niveles de test están descritos en [tests/README.md](tests/README.md).

## Estructura

```text
boot-protocol/   ABI BootInfo compartido por bootloader y kernel
libs/            librerías no_std con tests de host: elf, x86-paging, cmdline, fb-console
boot/            AriBoot: cargador UEFI (crate `ariboot`)
kernel/          AriKernel (crate `arikernel`), ELF64 en la mitad alta
tools/xtask/     automatización: build, imagen, QEMU, tests, lint
tests/qemu/      escenarios de integración declarativos (TOML)
docs/            arquitectura, flujo de trabajo y ADRs
build/           salida generada (ESP, OVMF_VARS, logs); no versionar
```

## Test de aceptación

`tests/qemu/boot_kernel_ready.toml` arranca QEMU sin pantalla con
`isa-debug-exit`, captura la serie y exige que aparezca
`ARICODEOS_KERNEL_READY`, que no haya `KERNEL PANIC` y que QEMU termine con
código 33. Los escenarios `full` inyectan fallos (ELF truncado, kernel
ausente, panic provocado) y exigen el código 35. Los logs quedan en
`build/scenarios/<nombre>/serial.log`.
