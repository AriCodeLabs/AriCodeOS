# AriCodeOS

Sistema operativo desde cero: microkernel en Rust `no_std`, arranque UEFI,
x86-64, seguridad por capabilities y drivers en espacio de usuario.

El documento maestro de arquitectura y roadmap es [AriCodeOS.md](AriCodeOS.md).
Las decisiones de diseño se registran en [docs/decisions/](docs/decisions/).

**Estado:** Milestone M0 "First Boot" completado. AriBoot (UEFI) carga
`kernel.elf`, construye las tablas de páginas, sale de boot services y salta a
AriKernel, que reporta el hardware por serie, pinta el framebuffer y emite
`ARICODEOS_KERNEL_READY`.

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
cargo xtask test           # test de integración boot.kernel_ready (exit code para CI)
cargo xtask check          # rustfmt --check + clippy -D warnings
```

Añade `--debug` a cualquier comando para usar el perfil de desarrollo.

## Estructura

```text
boot-protocol/   ABI BootInfo v1 compartido por bootloader y kernel
boot/            AriBoot: cargador UEFI (crate `ariboot`)
kernel/          AriKernel (crate `arikernel`), ELF64 en la mitad alta
tools/xtask/     automatización: build, imagen, QEMU, tests, lint
docs/            arquitectura y ADRs
build/           salida generada (ESP, OVMF_VARS, logs); no versionar
```

## Test de aceptación

`cargo xtask test` compila el kernel con la feature `qemu-exit`, arranca QEMU
sin pantalla con `isa-debug-exit`, captura la serie y exige tres cosas: que
aparezca `ARICODEOS_KERNEL_READY`, que no haya `KERNEL PANIC` y que QEMU
termine con código 33. El log queda en `build/test-serial.log`.
