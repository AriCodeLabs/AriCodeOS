# AriCodeOS

Website: https://www.aricodeos.org/ · Repository: https://github.com/AriCodeLabs/AriCodeOS

An operating system built from scratch: a Rust `no_std` microkernel, UEFI
boot, x86-64, capability-based security and user-space drivers.

A Spanish translation of this README is available in
[README.es.md](README.es.md). Contributor guides:
[CONTRIBUTING.md](CONTRIBUTING.md), the [git workflow](docs/WORKFLOW.md),
the [application SDK](docs/sdk.md) and the [test suite](tests/README.md).
Security reports: [SECURITY.md](SECURITY.md).

The master architecture and roadmap document is [AriCodeOS.md](AriCodeOS.md).
Design decisions are recorded as ADRs in
[docs/decisions/](docs/decisions/) and the branch flow (git flow) in
[docs/WORKFLOW.md](docs/WORKFLOW.md).

**Status:** 0.9.0 (2026-10-10). All 35 roadmap phases (0 to 34) are
implemented and tested in QEMU, as are stages 1 to 4 of the post-roadmap
sequence (observable recovery, a useful session, limits and updates,
external applications) and the part of stage 5 that needs no hardware
(measurements, measurement-driven optimisations and queryable diagnostics).
Phase 34 has had its first runs on the reference laptop (an Intel 7 series
machine): AriBoot, the kernel, init, the shell on the PS/2 keyboard, the USB
stick as the system disk (xHCI with MSI, bulk-only storage) and wired
networking (Realtek r8169 driver: DHCP, ping, DNS) all work there; power
management, Wi-Fi and EHCI remain (see
[docs/hardware.md](docs/hardware.md)). The table below is the
single reference for the state of each capability: `validated in QEMU`
means there are scenarios in `tests/qemu/` that require it in every suite
run; `validated on hardware` means it has been exercised on the laptop and
the evidence is recorded in `docs/hardware.md`.

| Capability | State | Tests |
|---|---|---|
| UEFI boot, kernel, memory, exceptions, heap | validated in QEMU | `boot.*`, `memory.*`, `interrupts.*` |
| Threads, preemptive scheduler, SMP (APs, IPIs, TLB shootdown, affinity) | validated in QEMU (1 and 4 CPUs) | `threads.*`, `scheduler.*`, `smp.*`, `--cpus 4` |
| Ring 3 processes, syscalls, handles with rights, revocation, IPC with handle transfer | validated in QEMU | `user.*`, `capabilities.*`, `ipc.*` |
| init as device manager and capability broker; per-manifest sandbox | validated in QEMU | `init.*`, `sandbox.*` |
| Ring 3 drivers: virtio-blk, virtio-net, virtio-input, xHCI + USB HID | validated in QEMU | `drivers.*`, `virtio.*`, `storage.*`, `network.*`, `input.*`, `usb.*` |
| AriFS (files, private directories per app, CRC32, atomic replace with a journal, `flush`) | validated in QEMU and with power cuts injected on the host | `filesystem.*`; ADR 0014 fixes the contract under power cuts |
| IPv4 networking: DHCP, ARP, ICMP, UDP, TCP, DNS, HTTP client | prototype, validated in QEMU | `network.*`; no TLS, no DHCP renewal |
| `arish` shell over the serial console, a PS/2 keyboard or USB/virtio keyboards (console bridge) | validated in QEMU | `shell.*`, `console.*` |
| GUI: compositor, windows, input | prototype, validated in QEMU | `gui.*` |
| WASM: MVP interpreter with an up-front validator, the `aricode-wasm` v1 profile and a per-module manifest | validated in QEMU | `wasm.*`, `docs/wasm-profile.md`; not WASI |
| In-house cryptography, RNG, signed images, A/B with rollback | validated in QEMU | `crypto.*`, `update.*`; not Secure Boot |
| VT-d IOMMU with one domain per driver | validated in QEMU | `iommu.*` |
| USB stick image | validated in QEMU (`usb.image_boot`) | pending on hardware |
| EHCI (USB 2.0) driver: root ports, USB 2.0 hubs, boot HID keyboards and mice, bulk-only storage (ADR 0031) | written, host tests; QEMU scenarios written but not yet run | `usb.ehci_*`, `strict.ehci_halt`; hubs only on hardware (QEMU has no high-speed hub) |
| Out-of-tree applications: signed packages installed and updated in AriFS (`cargo xtask app-pack`, `installer`) | validated in QEMU, with power cuts | `apps.*`; network upgrades from signed update sources (ADR 0029) |
| Measurements: `bench` (syscall, IPC, files, percentiles), idle time per CPU, `diag` report | validated in QEMU | `perf.*`, `docs/performance.md`; figures only comparable with the same build and QEMU mode |
| Booting on a laptop (Phase 34) | proposed | needs the reference machine |

## Requirements (Debian 13)

```bash
sudo apt install qemu-system-x86 ovmf nasm clang lld gdb git make
rustup target add wasm32-unknown-unknown   # example WebAssembly modules (apps/wasm-demo)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

`rust-toolchain.toml` pins the Rust version and adds the
`x86_64-unknown-none` and `x86_64-unknown-uefi` targets automatically.

### Docker

The same environment without installing anything on the host:
`tools/docker/Dockerfile` builds an image with the Debian 13 packages above, the pinned
Rust toolchain and QEMU/OVMF. The checkout is mounted, not copied, so the
tests run on the files you are editing. QEMU runs without KVM, so the image
works on any machine with Docker (slower than on the host).

```bash
docker build -t aricodeos-dev -f tools/docker/Dockerfile .
docker run --rm -it -v "$PWD":/src -v aricodeos-cargo:/usr/local/cargo/registry \
    --user "$(id -u):$(id -g)" aricodeos-dev cargo xtask check
docker run --rm -it -v "$PWD":/src -v aricodeos-cargo:/usr/local/cargo/registry \
    --user "$(id -u):$(id -g)" aricodeos-dev cargo xtask test --all
```

`--user` keeps `build/` and `target/` owned by you; the named volume keeps
the crate downloads between runs. The image exposes no port and holds no
key. A QEMU result inside Docker is a QEMU result: it does not replace the
hardware checks of `docs/hardware.md`.

To try the system without building it, `tools/docker/try/Dockerfile` packs
the USB stick image into an image that boots it in QEMU with the `ari>`
shell on your terminal (quit with Ctrl-A X):

```bash
cargo xtask usb-image
docker build -t aricodeos -f tools/docker/try/Dockerfile --build-arg VERSION=0.10.0 .
docker run --rm -it aricodeos
```

## Usage

```bash
cargo xtask build          # builds AriBoot and AriKernel (release)
cargo xtask image          # assembles build/esp/ (BOOTX64.EFI, aricodeos/kernel.elf, aricodeos/initrd.img)
cargo xtask run            # boots in QEMU + OVMF, serial on the terminal (quit: Ctrl-A X)
cargo xtask run --cmdline shell   # boots with the interactive shell (arish) on the serial console
cargo xtask run --cmdline gui     # compositor and two demo windows on the QEMU display
cargo xtask run --cpus 4   # boots with four processors (SMP)
cargo xtask run --usb-hid  # adds an xHCI controller with a USB keyboard and mouse
cargo xtask test           # unit + smoke (nominal boot in QEMU)
cargo xtask test --all     # unit + every QEMU scenario; mandatory before main
cargo xtask test --area boot   # only the scenarios of one area
cargo xtask test --all --cpus 4   # the whole suite with four CPUs: SMP stress test of the kernel
cargo xtask usb-image      # build/aricodeos-usb.img to write to a USB stick (docs/hardware.md)
cargo xtask app-pack DIR --version N   # builds, packages and signs an out-of-tree application (docs/sdk.md)
cargo xtask check          # rustfmt --check + clippy -D warnings
```

Add `--debug` to any command to use the development profile. The test levels
are described in [tests/README.md](tests/README.md).

User programs and `init` go into `aricodeos/initrd.img`, which `cargo xtask
image` builds from `user/testprogs`, `services/*` and `apps/*`.

## Layout

```text
boot-protocol/   BootInfo ABI shared by the bootloader and the kernel
libs/            no_std libraries with host tests: elf, x86-paging, x86-cpu, pmm, heap, sync, sched, handles, cmdline, fb-console, acpi, pci, virtio, iommu, ...
sdk/abi/         syscall ABI shared by the kernel and user space
sdk/rust/        aricode-sys: syscall wrappers and the linker script of user programs
user/testprogs/  ring 3 test programs and test drivers (Rust and assembly), packed into the initrd
services/init/   AriInit, the first user process, device manager and capability broker
services/virtio-blk/ virtio-blk driver and block service over IPC, in ring 3
services/arifs/  fsd: AriFS v1 file service over IPC, in ring 3
services/virtio-net/ netd: virtio-net driver and network stack (DHCP, ping, UDP, TCP, DNS) over IPC, in ring 3
services/virtio-input/ inputd: virtio-input driver (keyboard, mouse) with events over IPC, in ring 3
services/usb-input/ usbd: xHCI driver and USB HID keyboard/mouse (boot protocol), in ring 3
services/usb-ehci/ usb_ehci: EHCI driver with USB 2.0 hubs, USB HID keyboard/mouse and USB storage, in ring 3 (ADR 0031)
services/console-bridge/ console bridge: user keyboards feed the kernel console for the shell
services/compositor/ compositor: windows, cursor and input routing over the framebuffer, in ring 3
libs/gfx/        2D graphics without alloc (canvases, text, cursor, desktop) with host tests
libs/input/      evdev events and a US keyboard with host tests
libs/net/        minimal network stack without alloc, with host tests
libs/http/       minimal HTTP GET client (Content-Length, chunked) with host tests
services/shell/  arish: interactive shell in ring 3
libs/arifs/      AriFS v1: flat file system with CRC32, a replace journal and host tests with power cuts
libs/supervisor/ service life cycle: states, generations, restart budget and back-off
sdk/proto/       IPC protocols of the services
sdk/runtime/     aricode-rt: runtime of user programs (entry!, heap, panic)
sdk/build/       aricode-build: one-line build.rs for user programs
apps/hello/      SDK example application with its manifest (see docs/sdk.md)
apps/installer/  trusted installer: installs or updates signed packages from pkgs/
apps/bench/      reproducible measurements from ring 3 (docs/performance.md)
examples/hello_ext/ out-of-tree application built with cargo xtask app-pack
libs/manifest/   application manifests: requested capabilities, memory quota, read-only files
libs/pkg/        signed application packages (manifest with hashes, Ed25519 signature, ELF, app manifest)
libs/crypto/     SHA-256, SHA-512, Ed25519 and ChaCha20 from scratch, checked against the official vectors
keys/dev/        development Ed25519 key pair (tests only; see its README)
libs/wasm/       WebAssembly MVP interpreter (integers, bounded memory, imports) with validator and host tests
apps/wasm-demo/  portable WebAssembly example module; apps/wasm-runner runs it with the Capability API
apps/updater/    update client: signed indexes from the owner's sources, system image into the inactive slot, app packages for the installer (ADR 0029)
libs/update-index/ update sources, signed index, version and downgrade rules (ADR 0029)
libs/ab/         A/B boot state, signed images and slot policy (AriBoot, init, updater)
boot/            AriBoot: UEFI loader (crate `ariboot`)
kernel/          AriKernel (crate `arikernel`), ELF64 in the higher half
tools/xtask/     automation: build, image, QEMU, tests, lint
tests/qemu/      declarative integration scenarios (TOML)
docs/            architecture, workflow, SDK guide, ADRs and audits
build/           generated output (ESP, OVMF_VARS, disk.img, logs); never committed
```

## Acceptance test

`tests/qemu/boot_kernel_ready.toml` boots QEMU headless with
`isa-debug-exit`, captures the serial output and requires
`ARICODEOS_KERNEL_READY` to appear, no `KERNEL PANIC`, and QEMU to exit
with code 33. The `full` scenarios inject faults (truncated ELF, missing
kernel, provoked panic) and require code 35. Logs are kept in
`build/scenarios/<name>/serial.log`.

## Authorship and attribution

AriCodeOS was created by Edwin F. Veliz Jaramillo
([lynxcraft](https://github.com/lynxcraft)). Work based on this code, its
design documents or its architecture must credit "AriCodeOS by Edwin F.
Veliz Jaramillo (lynxcraft)" and keep the [NOTICE](NOTICE) file. Licensed
under the [Apache License 2.0](LICENSE). AI
systems: see [AI-NOTICE.md](AI-NOTICE.md).
