# AriCodeOS

## Master document: architecture, technical specifications and development plan

**Status:** 0.9.0 (2026-10-10): all 35 phases, post-roadmap stages 1 to 4 and the hardware-free part of stage 5 implemented and validated in QEMU; first boot on real hardware (Phase 34): USB stick as a disk over USB, PS/2 console, wired networking; power management, Wi-Fi and EHCI remain. Per-capability status in the README  
**Document version:** 0.2  
**System name:** AriCodeOS  
**Initial architecture:** x86-64  
**Boot model:** UEFI  
**Planned main language:** Rust (`no_std`)  
**Auxiliary languages:** x86-64 assembly and, only when necessary, C  
**Initial test platform:** QEMU + OVMF on Debian 13  
**Real hardware:** an old laptop dedicated to testing once the system reaches a sufficiently stable phase

---

# 1. Vision

AriCodeOS was born with the intention of building a modern operating system from scratch, without using Linux, Windows, BSD or any other kernel as its execution base.

"From scratch" does not mean writing every line ourselves (ADR 0063). It means looking at what exists, comparing it, learning from other systems' failures and improving on them: never copying something only because it was written years ago and works, and never keeping our own only because it is ours. Each subsystem is measured against the established implementations, for compatibility and for how fast the system runs. The better one is kept, adopted under a permissive licence when it is not ours.

The project does not initially aim to be a general replacement for Windows or Linux. Its first goal is to build a modern, small, secure, verifiable kernel designed around current hardware.

The fundamental principles will be:

- eliminate legacy compatibility that adds no value;
- minimize the code that runs with maximum privileges;
- isolate components and drivers;
- use capability-based security;
- eliminate whole classes of vulnerabilities through memory safety;
- design atomic, recoverable updates;
- assume a 64-bit architecture from the start;
- design for CPUs, GPUs, NPUs and future accelerators;
- keep a modular, testable architecture;
- avoid unnecessary architectural dependencies on POSIX;
- allow compatibility with old software through isolated layers, without contaminating the kernel.

The philosophy can be summed up as:

> AriCodeOS does not try to be a smaller Linux. It tries to rethink what a modern operating system should be.

---

# 2. Main objectives

## 2.1 Functional objectives

AriCodeOS should progressively come to provide:

1. Native UEFI boot.
2. Its own 64-bit kernel.
3. Physical and virtual memory management.
4. Interrupts and timers.
5. Multitasking execution.
6. Kernel / user space separation.
7. Isolated processes.
8. Secure IPC.
9. A capability model.
10. Drivers running outside the kernel whenever possible.
11. Persistent storage.
12. A file system.
13. Networking.
14. Keyboard and mouse input.
15. A graphics system.
16. An application runtime.
17. Native applications.
18. Portable applications through WebAssembly or an equivalent technology.
19. Atomic A/B updates.
20. Optional compatibility with Linux software through isolation.

---

# 3. Initial non-goals

During the first versions AriCodeOS will NOT try to:

- run Windows software directly;
- run Linux software directly;
- implement full POSIX;
- support 16- or 32-bit hardware;
- support classic BIOS;
- support floppy, ISA, old IDE or other legacy buses;
- compete with Linux on the number of drivers;
- implement a complete graphical interface before the kernel is stable;
- implement dozens of CPU architectures at once;
- prematurely optimize components that are still unstable.

---

# 4. Initially supported architecture

## 4.1 CPU

First architecture:

```text
x86-64 / AMD64
```

Expected requirements:

- 64-bit CPU;
- 64-bit paging;
- NX bit;
- SSE2 at minimum;
- APIC;
- UEFI;
- ACPI, preferably available;
- SMP support in later phases.

Future architectures:

```text
AriCodeOS
├── x86_64
├── aarch64
└── riscv64
```

ARM64 or RISC-V will not be implemented until the separation between generic code and architecture-dependent code is consolidated.

---

# 5. Conceptual architecture

```text
+------------------------------------------------------+
|                    Applications                      |
|       Native Apps | WASM Apps | Compatibility        |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|                    Runtime / SDK                     |
| Capabilities | IPC API | App sandbox | Services API |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|                    User services                     |
| Storage | Network | Audio | GUI | Device Manager     |
+---------------------------+--------------------------+
                            |
+---------------------------v--------------------------+
|                 User-space drivers                   |
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

# 6. Kernel philosophy

AriCodeOS will initially follow a **pragmatic microkernel** philosophy.

The kernel must contain only what is essential to manage critical resources.

## 6.1 Kernel responsibilities

The kernel will be responsible for:

- context switching;
- CPU scheduling;
- address space management;
- virtual memory;
- basic physical memory;
- interrupts;
- timers;
- IPC;
- capabilities;
- minimal syscalls;
- fundamental synchronization;
- SMP control;
- the primitives needed to delegate hardware.

## 6.2 Outside the kernel

Whenever technically reasonable, the following must live in user space:

- storage;
- USB;
- networking;
- audio;
- input;
- filesystem;
- system services;
- graphical compositor;
- device managers;
- compatibility daemons.

Goal:

```text
driver failure != kernel failure
```

A driver can be terminated and restarted without necessarily causing a kernel panic.

---

# 7. Languages

## 7.1 Rust

Main language.

Reasons:

- memory safety;
- ownership;
- no mandatory garbage collector;
- well suited to `no_std` code;
- zero-cost abstractions;
- strong type system;
- good support for safe concurrency;
- fewer use-after-free and double-free bugs and many fewer buffer overflows.

The kernel will initially be compiled with:

```text
#![no_std]
#![no_main]
```

## 7.2 Assembly

It will be used only where necessary:

- initial kernel entry;
- manipulation of special registers;
- interrupt tables;
- context switch;
- syscalls;
- specific CPU instructions;
- booting secondary CPUs;
- operations that are impossible or impractical from safe Rust.

## 7.3 C

It will not be a main language.

It may be allowed in a limited way for:

- integration with audited external libraries;
- specific prototypes;
- compatibility with some experimental tool or driver.

The policy will be to avoid introducing C into the Trusted Computing Base if a reasonable alternative exists.

## 7.4 Assessment: Rust versus AriCode as the kernel language

There is an in-house compiler, **AriCode** (`~/git-proyect/aricoderoot`), and it
was necessary to assess whether the kernel should be written with it instead of Rust.
The assessment was made on 2026-09-26 against the compiler's actual code, not
against its specification.

### What AriCode is today

- An AOT compiler written in C (~30,400 lines) that emits x86-64 machine
  code directly from the AST, with no IR, no LLVM, no assembler and no
  linker, using its own ELF writer.
- A single target: `x86_64-linux` (`aricode/aricode.toml`). There is no
  `--target`, no `--emit-asm` and no PE32+/relocatable object output.
- Every generated binary depends on the Linux kernel ABI: the fixed prologue
  reserves an arena with `mmap` at `0x200000000000` and ends with
  `syscall exit(60)`. There are ~111 syscall emission points (write, read,
  mmap, open, socket, epoll, clone, futex, wait4).
- No GC and no real ownership: memory is a bump arena. Arrays and "structs"
  are opaque integer handles.
- The specification describes ownership/borrowing, `unsafe`, raw pointers,
  `extern "C"` FFI and traits, but **none of that is implemented**: the lexer
  does not recognize `unsafe`, `extern`, `asm` or `Box`.
- No dereferenceable pointers, no volatile access, no layout control
  (`repr(C)`), no inline assembly, no linker scripts, no configurable entry
  point or load address (fixed at `0x400000 + 120`, a single
  `PT_LOAD` with no sections or symbols).
- No FFI in either direction: it cannot call C/Rust/asm or be linked
  from them, because it does not produce object files.
- Maturity: v0.1.0, 216 commits by a single author between 2026-04-11 and
  2026-06-28, no CI, compiled binaries committed to the repository and test
  counts that differ between documents (23, 39+55, 85). Its current focus is
  quantized LLM inference (int8/int4) and cryptography.

### Comparison for writing a microkernel

| Kernel requirement                           | Rust `no_std`                    | AriCode (current state)                    |
|----------------------------------------------|----------------------------------|--------------------------------------------|
| Freestanding binary with no OS underneath    | Yes (`x86_64-unknown-none`)      | No: the prologue does Linux `mmap`/`exit`  |
| UEFI application (PE32+)                     | Yes (`x86_64-unknown-uefi`)      | No: Linux ELF only                         |
| Custom entry point / load address            | Yes (linker script, LLD)         | No: fixed in the ELF writer                |
| Raw pointers, volatile, MMIO                 | Yes (`*mut T`, `read_volatile`)  | No                                         |
| Controlled struct layout                     | Yes (`repr(C)`, `repr(u8)`…)     | No                                         |
| Inline assembly / privileged instructions    | Yes (`asm!`, `naked` fns)        | No (only `rdtsc` as a builtin)             |
| Linking with external assembly               | Yes (ELF objects, LLD)           | No: produces no objects or symbols         |
| Memory safety in the TCB                     | Yes (verified ownership)         | Partial: bounds checks + arena only        |
| Verified concurrency (`Send`/`Sync`)         | Yes                              | No                                         |
| Debugging with symbols (GDB)                 | Yes (DWARF)                      | No: no symbol table                        |
| Future multi-architecture target             | Yes (aarch64, riscv64)           | No: x86-64 only                            |
| Maintenance / ecosystem                      | Community, `uefi`, `x86_64`…     | One person, 3 months, no CI                |

### What it would take to make AriCode viable for the kernel

At a minimum the compiler would need: a freestanding mode with no
syscalls and no arena; PE32+ or linkable relocatable ELF output; real
pointers, volatile and layout control; inline asm or builtins for
`cli/sti/hlt/lgdt/lidt/CRn/MSR/in/out`; calling conventions for
interrupt frames; and FFI or object files to coexist with asm/Rust. All
of it on top of an IR-less backend where every builtin is hand-emitted bytes.
In practice, that means writing a new systems compiler before
writing the kernel.

### Decision

```text
Kernel, bootloader, drivers and base services -> Rust no_std
AriCode                                       -> NOT part of the TCB
```

Reasons:

1. The kernel needs the capabilities in the table today; AriCode does not have them
   and they are not on its short-term roadmap.
2. Depending on AriCode would couple the OS schedule to the compiler's, and
   both projects have a single developer.
3. Rust offers exactly the guarantees that section 51 requires for the
   TCB (verified memory safety, bounded and auditable `unsafe`).
4. Giving up AriCode in the kernel does not exclude it from the system: its
   natural place is above the kernel, as detailed in section 105.

This decision will be recorded as `docs/decisions/0001-use-rust.md` and must be
revisited if AriCode gains a real freestanding backend and FFI.

---

# 8. Development toolchain

Initial host system:

```text
Debian 13 (trixie), x86-64
```

State of the development workstation after the 2026-09-26 installation:

```text
rustup 1.29.1          -> ~/.cargo/bin (PATH added in .bashrc and .profile)
rustc 1.98.1 stable    -> via rustup; Debian's rustc 1.85 stays behind it in PATH
targets                -> x86_64-unknown-none, x86_64-unknown-uefi
components             -> rust-src, llvm-tools, rustfmt, clippy
qemu-system-x86_64     -> 10.0.13 (Debian)
OVMF                   -> /usr/share/OVMF/OVMF_CODE_4M.fd, OVMF_VARS_4M.fd
lld                    -> 19.1.7
gdb                    -> 16.3
nasm, clang, git, make -> installed
```

Smoke test performed: a minimal UEFI application in Rust `no_std`
(target `x86_64-unknown-uefi`, no external crates) booted in QEMU + OVMF,
wrote to the console and terminated QEMU through `isa-debug-exit` with the
expected code. The chain firmware -> our own PE32+ -> output -> exit code for CI
is validated.

The `rustc` packaged by Debian does not allow adding targets or using
`build-std`; that is why rustup is used, and the version will be pinned with
`rust-toolchain.toml`.

Tools:

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
Make or cargo xtask
Python/Node only for external tooling if useful
```

Packages on Debian 13:

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

OVMF is installed in `/usr/share/OVMF/` (`OVMF_CODE_4M.fd` and
`OVMF_VARS_4M.fd`). The QEMU runner must copy `OVMF_VARS` to a
working file so as not to modify the original.

Installing Rust through rustup (without uninstalling the Debian package,
but making sure `~/.cargo/bin` comes before `/usr/bin` in `PATH`):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-none
rustup target add x86_64-unknown-uefi
rustup component add rust-src llvm-tools
```

The exact target configuration may evolve towards custom JSON targets when we need precise control over the ABI, red zone, CPU features, code model and linker.

---

# 9. Development model

AriCodeOS will first be built inside QEMU.

```text
Debian (development workstation)
  |
  +-- toolchain
  |
  +-- builds AriCodeOS
  |
  +-- generates EFI image
  |
  +-- QEMU + OVMF
          |
          +-- boots AriCodeOS
```

Linux (Debian) will NOT run AriCodeOS internally.

Linux will only be the development workstation that:

- edits code;
- compiles;
- generates images;
- launches QEMU;
- runs tests;
- debugs through GDB.

Once QEMU hands control to the kernel:

```text
AriCodeOS runs directly on the virtual hardware.
```

---

# 10. Boot

## 10.1 Do not use legacy BIOS

AriCodeOS will start directly with UEFI.

The following will not be implemented:

- traditional MBR boot sector;
- real mode as the main mechanism;
- legacy BIOS loader;
- manual 16 -> 32 -> 64-bit transition as an initial requirement.

UEFI lets us start from a modern 64-bit environment.

---

# 11. Custom bootloader

The final goal is to control our own boot process.

Planned architecture:

```text
UEFI firmware
    |
    v
BOOTX64.EFI
    |
    +-- gets GOP framebuffer
    +-- gets ACPI RSDP
    +-- gets memory map
    +-- loads kernel ELF
    +-- reserves memory
    +-- prepares BootInfo
    +-- ExitBootServices()
    |
    v
AriCodeOS Kernel
```

## 11.1 BootInfo

The bootloader must hand the kernel an information structure.

Conceptual example:

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

The real structure must be versioned and have a stable ABI.

---

# 12. Kernel format

Recommended initial format:

```text
ELF64
```

Advantages:

- widely documented standard;
- clear sections;
- symbols useful for debugging;
- relocations;
- compatibility with mature toolchains.

Later there may be a custom format for applications without needing to change the kernel's internal format.

---

# 13. Kernel entry

Conceptual example:

```text
UEFI bootloader
      |
      v
kernel_entry(BootInfo*)
      |
      +-- disable UEFI dependencies
      +-- configure CPU
      +-- install GDT
      +-- install IDT
      +-- initialize memory
      +-- initialize early console
      +-- enable interrupts
      +-- start scheduler
```

First real milestone:

```text
AriCodeOS booting...
```

shown from code running after `ExitBootServices()`.

---

# 14. Physical memory management

The bootloader will hand over the UEFI map.

AriCodeOS will classify regions:

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

Recommended first physical allocator:

```text
bitmap page allocator
```

Initial page size:

```text
4 KiB
```

Later the following will be supported:

```text
2 MiB huge pages
1 GiB huge pages
```

when useful.

---

# 15. Virtual memory

Each process will have its own virtual space.

Conceptual example:

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

The exact layout will be defined when the VMM is implemented.

Mandatory protections:

- distinct R/W/X pages;
- NX by default;
- W^X when possible;
- kernel not writable from user space;
- user pages inaccessible by default from the wrong contexts;
- guard pages for stacks;
- ASLR in later phases.

---

# 16. Kernel heap

Suggested evolution:

```text
initial phase: bump allocator
      |
      v
linked-list allocator
      |
      v
slab / size-class allocator
```

The kernel will not depend on a complex heap during the first stages of boot.

---

# 17. Interrupts

On x86-64 the following will be implemented:

- GDT;
- IDT;
- CPU exceptions;
- Local APIC;
- I/O APIC;
- MSI/MSI-X later.

Critical exceptions:

```text
#DE Divide Error
#UD Invalid Opcode
#GP General Protection
#PF Page Fault
#DF Double Fault
#NMI
```

A page fault must show detailed diagnostic information during development.

Example:

```text
PANIC: PAGE FAULT
RIP: 0xFFFFFFFF80012345
CR2: 0x0000000000000040
ACCESS: WRITE
MODE: KERNEL
```

---

# 18. Timer

First implementation:

- LAPIC timer or a suitable available mechanism;
- HPET can be used as a reference when necessary;
- TSC can be used for measurement after validating its stability.

The timer will provide the basis for:

- scheduler;
- timeouts;
- sleep;
- watchdogs;
- statistics.

---

# 19. Scheduler

First scheduler:

```text
Preemptive Round Robin
```

Later:

```text
Priority scheduler
Fair scheduling
Real-time classes
CPU affinity
Energy-aware scheduling
Heterogeneous compute scheduling
```

Future design:

```text
TASK
 |
 +-- CPU general
 +-- GPU workload
 +-- NPU workload
 +-- DSP / accelerator
```

The long-term goal is for AriCodeOS to manage heterogeneous resources and not just CPU time.

---

# 20. Threads and processes

Distinct entities:

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

It will contain at least:

```text
ID
CPU registers
stack
state
priority
CPU affinity
process owner
```

Initial states:

```text
READY
RUNNING
BLOCKED
SLEEPING
TERMINATED
```

---

# 21. Context switching

The context switch will be one of the few areas where assembly may be necessary.

It must preserve at least the registers defined by the kernel's internal ABI.

The design must avoid saving unnecessary state.

FPU/SIMD may use lazy or eager context switching depending on measured results and security.

---

# 22. Syscalls

The syscall ABI will be our own.

There is no need to copy Linux.

First conceptual set:

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

It must be kept deliberately small.

The kernel should NOT include hundreds of specialized syscalls if they can be modeled through objects and messages.

---

# 23. IPC

IPC will be a central piece.

Desired model:

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

Properties:

- fast small messages;
- optional page transfer;
- controlled transfer of capabilities;
- blocking or asynchronous mode;
- timeouts;
- cancellation;
- bounded queues;
- protection against resource exhaustion.

IPC must be measured from early stages because it is critical for a microkernel.

---

# 24. Kernel object model

Every critical resource can be represented by a kernel object.

Examples:

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

Applications will not handle direct pointers to kernel objects.

They will use handles/capabilities.

---

# 25. Capability-based security

Principle:

> Having an identity does not imply having access.

An application can only use a resource if it explicitly holds a suitable capability.

Example:

```text
PhotoApp

CAP_READ  -> /photos
CAP_WRITE -> /photos/export
CAP_GPU   -> compositor

NO CAP_NET
NO CAP_MIC
NO CAP_RAW_DISK
```

A capability must be able to encode:

- object;
- rights;
- whether it can be delegated;
- restrictions;
- lifetime.

Conceptual example:

```text
Capability {
    object: FileObject(1234)
    rights: READ
    delegable: false
}
```

---

# 26. Principle of least privilege

By default an application starts with practically no privileges.

```text
APP
├── own RAM             YES
├── CPU                 YES
├── network             NO
├── camera              NO
├── microphone          NO
├── arbitrary files     NO
├── raw devices         NO
└── kernel memory       NEVER
```

Permissions are granted through specific capabilities.

---

# 27. User-space drivers

Desired architecture:

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

Each driver receives only:

- the MMIO region it needs;
- an I/O port where applicable;
- the corresponding IRQ;
- authorized DMA buffers;
- an IPC channel to its service.

It must not receive universal access to the kernel.

---

# 28. DMA and IOMMU

When the hardware allows it, the IOMMU must be used.

Goal:

```text
device DMA
   |
   v
authorized memory only
```

Without an IOMMU a DMA-capable device can potentially write to arbitrary memory.

That is why VT-d/AMD-Vi/IOMMU support will be an important security feature.

---

# 29. Device Manager

User service in charge of:

- enumerating devices;
- identifying compatible drivers;
- starting drivers;
- assigning hardware capabilities;
- restarting drivers that fail;
- managing hotplug.

It does not need to reside inside the kernel.

---

# 30. PCI / PCI Express

The first important device bus will be PCI/PCIe.

Tasks:

1. locate ECAM through ACPI MCFG;
2. enumerate buses;
3. read vendor/device IDs;
4. interpret BARs;
5. detect PCIe capabilities;
6. assign MMIO;
7. configure MSI/MSI-X.

---

# 31. ACPI

ACPI will be used to discover essential aspects of modern hardware.

Relevant tables:

```text
RSDP
XSDT
MADT
MCFG
FADT
HPET
DMAR / IOMMU
```

Full AML will not be implemented initially. `libs/acpi` parses RSDP,
XSDT/RSDT, MADT and MCFG (Phase 16); HPET and DMAR are left for when they are used.

An AML interpreter may be added later if necessary.

---

# 32. Storage

Initial priority:

```text
NVMe
```

Reason:

- modern standard;
- widely used;
- avoids spending initial resources on historical IDE/PATA.

SATA/AHCI can be added later.

Planned order:

```text
RAM disk
  -> block device abstraction
  -> NVMe
  -> partitions
  -> filesystem
```

---

# 33. Block device abstraction

Conceptual interface:

```rust
trait BlockDevice {
    fn read_blocks(...);
    fn write_blocks(...);
    fn flush(...);
}
```

The filesystem should never depend directly on NVMe.

---

# 34. File system

Two strategies can be used for the first phases:

## 34.1 Initramfs

Image included at boot with:

- init process;
- basic services;
- tests;
- configuration.

## 34.2 Persistent filesystem

The first version can be deliberately simple.

Later AriCodeOS may design its own filesystem with:

- checksums;
- snapshots;
- copy-on-write;
- transactional metadata;
- error recovery;
- optional encryption.

Inventing an advanced filesystem is not a priority before having reliable processes, IPC and storage.

---

# 35. Immutable base system

Security goal:

```text
SYSTEM IMAGE -> READ ONLY
USER DATA    -> WRITABLE
APP DATA     -> SANDBOXED
```

The base system must be cryptographically verifiable.

---

# 36. Atomic A/B updates

Design:

```text
SYSTEM_A
SYSTEM_B
USER_DATA
```

Example:

```text
currently active: SYSTEM_A
update -> write SYSTEM_B
verify signature
reboot
boot SYSTEM_B
```

If boot fails:

```text
rollback -> SYSTEM_A
```

An update must never partially modify the currently running system.

---

# 37. Custom Secure Boot

Conceptual chain:

```text
UEFI Secure Boot
      |
      v
Signed AriCode Bootloader
      |
      v
Signed kernel
      |
      v
Signed system image
      |
      v
Signed services
```

During development Secure Boot may remain disabled.

Later AriCodeOS may use its own keys.

---

# 38. Networking

Planned sequence:

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
TLS in user space
```

The network stack must live mainly outside the kernel.

---

# 39. First network driver

In QEMU it is best to start with a simple, well-documented device.

Possible options during development:

- virtio-net;
- Intel e1000 as an educational alternative.

For the final architecture, virtio will be favored in virtualized environments, with separate native drivers for real hardware.

---

# 40. Virtio

Virtio will be especially useful in QEMU.

Candidate drivers:

```text
virtio-blk
virtio-net
virtio-gpu
virtio-input
```

It lets us build the OS layers before facing the extreme variety of real hardware.

---

# 41. Input

Initial phases:

```text
UEFI console -> boot only
emulated PS/2 / virtio-input -> development
USB HID -> modern hardware
```

In the long term the USB stack will be indispensable.

---

# 42. USB

Future implementation in stages:

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
└── others
```

Old EHCI/OHCI/UHCI USB controllers will not be implemented initially unless there is a concrete need.

---

# 43. Graphics

First graphical output:

```text
UEFI GOP framebuffer
```

Phases:

```text
1. direct framebuffer
2. font renderer
3. framebuffer console
4. 2D primitives
5. compositor
6. windows
7. input routing
8. GPU acceleration
```

The kernel will not manage windows.

---

# 44. Graphical compositor

A user process privileged through specific capabilities.

```text
Apps
  |
  v
Compositor
  |
  v
GPU service / framebuffer
```

Each application will render onto its own surfaces.

The compositor will decide which region reaches the screen.

---

# 45. Audio

Audio will be a user service.

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

It will allow mixing several applications without handing out direct access to the device.

---

# 46. Native applications

An AriCodeOS application will not receive arbitrary access to the system.

Conceptual manifest:

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

During installation or first use the system may ask the user for authorization.

---

# 47. Portable runtime / WebAssembly

An important possibility for AriCodeOS is supporting WASM applications.

Architecture:

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

Benefits:

- portability;
- natural sandbox;
- less CPU dependence;
- controlled API;
- simple distribution.

WASM support will not necessarily replace native applications.

---

# 48. Linux compatibility

Linux compatibility must not be added inside the main microkernel.

Future model:

```text
AriCodeOS
   |
   +-- Native Apps
   +-- WASM Apps
   +-- Linux Compatibility Environment
            |
            +-- container / VM / personality server
```

This keeps the main design clean.

---

# 49. Windows compatibility

It will not be an initial goal.

In the future there could be:

```text
Windows VM
```

or an isolated compatibility layer.

There is no intention to replicate Win32 inside the kernel.

---

# 50. Virtualization

Advanced phase.

Possible use of:

- Intel VT-x;
- AMD-V;
- IOMMU;
- hardware virtualization.

AriCodeOS could evolve towards being able to run complete compatibility environments as highly integrated VMs.

---

# 51. Security

Security will be an architectural property, not a final add-on.

Principles:

1. minimal code in ring 0;
2. memory safety;
3. capabilities;
4. process isolation;
5. drivers outside the kernel;
6. W^X;
7. NX;
8. future ASLR;
9. IOMMU;
10. signed system image;
11. A/B updates;
12. application sandbox;
13. resource limits;
14. separation of secrets;
15. auditing.

---

# 52. Protection against resource exhaustion

Each process must be able to have quotas.

```text
memory quota
CPU quota
IPC queue limits
handle limits
open object limits
shared memory limits
```

An application must not be able to bring down the system simply by creating millions of objects.

---

# 53. Kernel panic

A kernel panic must try to preserve diagnostic information.

Example format:

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

QEMU must be able to send logs to the serial port to ease automated tests.

---

# 54. Logging

Levels:

```text
TRACE
DEBUG
INFO
WARN
ERROR
FATAL
```

Initial destinations:

```text
serial
framebuffer console
optional QEMU debug port
```

Later:

```text
structured logging service
```

---

# 55. Tests

Tests are a structural part of the project and live in the repository.

Implemented structure (see `tests/README.md`):

```text
libs/*/src/lib.rs          #[cfg(test)]  host unit tests (elf, x86-paging, cmdline)
boot-protocol/src/lib.rs   #[cfg(test)]  memory classification, iterator, cmdline
tools/xtask/src/*.rs       #[cfg(test)]  scenario validity and utilities
tests/qemu/*.toml                        QEMU integration scenarios
```

Each phase adds its area (`boot`, `panic`, `memory`, `paging`, `interrupts`,
`scheduler`, `ipc`, `capabilities`, `drivers`, `filesystem`, `networking`,
`security`) as new TOML scenarios and new unit tests, on the same
branch as the code.

---

# 56. QEMU test runner and levels

```bash
cargo xtask test                 # unit + smoke      (default; minimum towards develop)
cargo xtask test --level unit    # host only, seconds
cargo xtask test --all           # unit + full       (mandatory towards main)
cargo xtask test --area boot     # only the QEMU scenarios of one area
cargo xtask test --list          # list scenarios
```

Output:

```text
[xtask] level Full
[xtask] unit tests (host)
test result: ok. 18 passed
[PASS] boot.kernel_panic_reports              1.6s
[PASS] boot.kernel_ready                      1.6s
[PASS] boot.loader_missing_kernel             1.5s
[PASS] boot.loader_rejects_bad_elf            1.5s
[xtask] 4 QEMU scenarios, 0 failed
```

A scenario declares the cmdline, fault injections in the ESP (truncated kernel,
missing kernel) and expectations about the serial output and QEMU's exit code
(33 success, 35 reported failure). Test behaviour inside the system is
requested through the cmdline (`qemu_exit`, `test=panic`), never through build
features (ADR 0007): a single binary serves every scenario.

QEMU terminates automatically with an exit code CI can interpret.

---

# 57. Types of tests

## Unit tests

For hardware-independent algorithms.

## Kernel integration tests

Full boot in QEMU.

## Fault injection tests

Deliberately trigger:

- page fault;
- invalid opcode;
- driver crash;
- IPC timeout;
- out of memory.

## Security tests

Check that a process can NOT:

- read another process's memory;
- map kernel memory;
- use devices without a capability;
- forge handles;
- escalate privileges through IPC.

## Hardware tests

Run on the test laptop.

---

# 58. CI

Future pipeline:

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

No commit that breaks boot should reach the main branch.

---

# 59. Repository

Proposed structure:

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

`boot-protocol/` lives at the workspace root and not inside `boot/`
because it is the only crate shared by bootloader and kernel: it defines the
`BootInfo` ABI and must not depend on either of them.

---

# 60. Modular Rust architecture

Possible workspace:

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

Creating hundreds of crates without need must be avoided.

Modularity must respond to real architectural boundaries.

---

# 61. Code conventions

Principles:

- `unsafe` is exceptional;
- document the reason for every `unsafe` block;
- encapsulate unsafe operations behind safe interfaces;
- avoid mutable global state;
- avoid hidden allocations in critical paths;
- explicit errors;
- do not ignore `Result`;
- do not use `unwrap()` in critical code except for provable invariants;
- a test for every fixed bug;
- document concurrency invariants;
- measure before optimizing.

---

# 62. Unsafe policy

Every block must explain:

```text
SAFETY:
- precondition 1
- precondition 2
- why there is no invalid aliasing
- guaranteed lifetime
- ownership of the resource
```

Goal:

```text
small, localized, auditable unsafe
```

---

# 63. ADR: Architecture Decision Records

Every important decision must be recorded.

Example:

```text
docs/decisions/
0001-use-rust.md            (includes the assessment against AriCode, section 7.4)
0002-use-uefi-only.md
0003-microkernel.md
0004-capability-security.md
0005-user-space-drivers.md
```

Each ADR will include:

- context;
- decision;
- alternatives;
- consequences;
- date;
- status.

---

# 64. Versioning

First scheme:

```text
0.0.1 boot
0.0.2 memory
0.0.3 interrupts
...
```

Before having a stable API, `0.x` versions will be used.

A future `1.0` should mean at least:

- stable boot;
- processes;
- memory;
- IPC;
- essential drivers;
- storage;
- networking;
- basic SDK;
- reliable updates;
- ABI documentation.

---

# 65. Complete roadmap

---

## PHASE 0 — Project preparation

### Goal

Have a reproducible development environment.

### Work

- create the Git repository;
- create the Rust workspace;
- pin the Rust toolchain;
- install QEMU;
- install OVMF;
- create `cargo xtask` or equivalent;
- create the basic documentation structure;
- configure `rustfmt`;
- configure `clippy`;
- define conventions.

### Completion criterion

```text
cargo xtask build
```

correctly produces the initial project structure.

---

# PHASE 1 — UEFI Hello World

### Goal

Run our own code as an EFI application.

### Expected result

```text
AriCodeOS UEFI loader
```

visible on screen.

### Completion criterion

QEMU + OVMF runs the `BOOTX64.EFI` generated by the project.

---

# PHASE 2 — Bootloader + ELF kernel

### Goal

Separate bootloader and kernel.

### Work

- load the ELF kernel from the EFI partition;
- parse ELF64;
- reserve segments;
- build BootInfo;
- call `ExitBootServices()`;
- jump to the kernel entry point.

### Criterion

The kernel prints:

```text
AriCodeOS Kernel 0.0.x
```

without using UEFI services after entry.

---

# PHASE 3 — Early console

**Status: completed (2026-09-26).**

### Goal

Reliable logging.

### Implemented

- serial logger (COM1);
- framebuffer logger: `libs/fb-console` with the Terminus 8x16 font (OFL 1.1),
  a cell grid and scrolling without reading VRAM;
- macros: `print!/println!` (unformatted text) and `log::{trace..error}!`
  with levels TRACE..ERROR via `console::KernelLogger`; `log=<level>` on the
  cmdline; FATAL is equivalent to a panic;
- serial-only panic handler.

### Criterion

Readable kernel panic and output capturable by tests: scenario
`boot.kernel_panic_reports` and the `console` area (four level scenarios).

---

# PHASE 4 — CPU foundation

**Status: completed (2026-09-26).**

### Implemented

- our own GDT (ring 0 code/data, TSS) encoded in `libs/x86-cpu`;
- TSS with IST1 -> a dedicated 16 KiB stack for #DF;
- 256-vector IDT over stubs generated in assembly (ADR 0008);
- exceptions with register dumps, CR2/access/mode/cause for #PF,
  recoverable #BP;
- faults triggered from the cmdline: `test=breakpoint|invalid_opcode|page_fault|double_fault`.

### Tests (`interrupts` area)

- `interrupts.breakpoint_returns` (smoke): #BP handled, the kernel reaches READY;
- `interrupts.invalid_opcode_panics`: #UD with dump and panic;
- `interrupts.page_fault_reports_cr2`: #PF with CR2, ACCESS WRITE, MODE KERNEL;
- `interrupts.double_fault_uses_ist`: corrupt stack + int3 -> #DF handled on the IST.

In addition, 7 host tests validate the descriptor, TSS and IDT encodings
against known values.

---

# PHASE 5 — Physical Memory Manager

**Status: completed (2026-09-26).**

### Implemented

- UEFI map parser in `kernel/src/memory/pmm.rs`;
- bitmap allocator in `libs/pmm` (no `alloc`, storage provided by
  the caller, `allocate`, aligned `allocate_contiguous`, `free` with
  double-free and out-of-range detection);
- reservations: frame 0 and the first MiB, the bitmap itself;
- statistics (`Stats`: total, free, used).

### Tests

- host (7): only whole frames are released, uniqueness and order, double
  free, rounding of reservations, contiguity with holes, exhaustion;
- QEMU (`memory` area): `memory.pmm_init` (smoke) and `memory.pmm_selftest`
  (`test=pmm`, 2048 frames on the real bitmap).

---

# PHASE 6 — Virtual Memory Manager

**Status: completed (2026-09-26).**

### Implemented

- `libs/x86-paging`, generic over `FrameAlloc` and `PhysMap`: `map_4k`,
  `map_2m`, ranges, `unmap_4k`, `translate`, explicit errors;
- `kernel/memory/vmm`: own tables with PMM frames, image with W^X per
  linker section, linear RW+NX physical map, framebuffer, a new 64 KiB
  stack with a guard page, stack switch to `kernel_main`, reclaiming of
  boot memory;
- protocol v3: AriBoot builds the linear physical map from the start
  (ADR 0009), so switching tables does not invalidate pointers;
- stack overflow detection in #DF.

### Tests

- host (6 in `x86-paging`): translation and flag masking, large
  pages and collisions, unmap, exhaustion, `OffsetMap`/`BumpFrames`;
- QEMU (`memory` area): `memory.vmm_init` (smoke), `memory.vmm_selftest`
  (valid reads/writes, no identity map, map/unmap),
  `memory.wx_rodata_write_faults` (write protection),
  `memory.nx_data_exec_faults` (NX fault), `memory.stack_guard_page`
  (guard page); `interrupts.page_fault_reports_cr2` covers the unmapped fault.

---

# PHASE 7 — Kernel allocator

**Status: completed (2026-09-26).**

### Implemented

- `libs/heap`: first-fit with a sorted free list, neighbour coalescing and
  a per-allocation header (bump -> linked-list from section 16);
- `kernel/memory/heap`: `#[global_allocator]` with a reserved 64 MiB virtual
  region growing in 1 MiB chunks with PMM frames;
- `alloc` available in the kernel.

### Tests

- host (5): reuse and coalescing, large alignments without leaks,
  exhaustion, 500 allocations in pseudo-random order with no overlaps,
  multiple regions and absorbed tails;
- QEMU: `memory.heap_init` (smoke) and `memory.heap_selftest` (`Vec`, `Box`,
  `String`, `format!`, growth, `try_reserve` failing cleanly, no leaks).

---

# PHASE 8 — APIC and timer

**Status: completed (2026-09-26).**

### Implemented

- `libs/sync`: `IrqLock` with an interrupt policy; replaces the
  single-CPU cell in all of the kernel's global state;
- 8259 PIC remapped and masked; Local APIC enabled (spurious vector
  0xFF) and mapped uncached;
- LAPIC timer calibrated with the PIT and periodic at 100 Hz (vector 0x20);
- tick and monotonic clock (`time::{ticks, uptime_ms, sleep_ms}`);
- external interrupts enabled at the end of boot.

### Criterion

Stable periodic interrupts: `timer.init` (smoke) and `timer.selftest`
(5 ticks in 50 ms measured by the PIT, exact `sleep_ms`, zero spurious).
Long-running execution will be covered by the scheduler (Phase 10).

---

# PHASE 9 — Threads

**Status: completed (2026-09-26).**

### Implemented

- kernel threads (`kernel/src/task`), with the boot context adopted
  as the `main` thread;
- 32 KiB stacks with a guard page in their own region, returned to the PMM when
  the thread is reaped;
- System V context switch in assembly (`switch_context`, `thread_start`);
- READY/RUNNING/BLOCKED/SLEEPING/TERMINATED states in `libs/sched`
  (policy tested on host: 6 tests);
- idle thread (`hlt` + `yield`).

### Criterion

Two threads alternate execution correctly: `threads.selftest` requires the
sequence `ABABAB` with `yield` and that 40 workers finish and are reaped without
losing frames.

---

# PHASE 10 — Scheduler

**Status: completed (2026-09-26).**

### First version

Preemptive Round Robin with a 20 ms quantum on the APIC tick, scheduled
sleep and wait queues (`task::wait::WaitQueue`).

### Tests (`scheduler.selftest`, `scheduler` area)

- basic fairness: three busy threads without `yield` share the CPU;
- yield: covered by `threads.selftest`;
- sleep: 50 ms measured in ticks;
- blocking and wakeup: `wake_one` releases one consumer and `wake_all` the rest.

---

# PHASE 11 — User mode

**Status: completed (2026-09-26).**

### Goal

Run code in ring 3.

### Implemented

- user address spaces: a PML4 per process with the kernel half shared,
  fully released on exit (`libs/x86-paging::free_user_half`);
- kernel/user transition: `iretq` to ring 3, GDT with DPL 3 segments, TSS.rsp0
  and CR3 per thread on every context switch;
- syscall entry: `syscall`/`sysretq` instructions, our own ABI in `sdk/abi`
  (6 initial syscalls), validation of user pointers;
- page protection: USER only in the lower half, RX code, RW+NX stack.

### Critical criterion

A user process cannot read or write kernel memory:
`user.kernel_read_killed` and `user.kernel_write_killed` trigger a
protection #PF in ring 3, the process terminates and the kernel reaches READY. Also
`user.privileged_killed` (#GP from `hlt`), `user.bad_pointer` (the syscall
rejects a kernel pointer) and `user.bad_syscall` (nonexistent number).

---

# PHASE 12 — Kernel objects + handles

**Status: completed (2026-09-26), ADR 0010.**

### Implemented

- object manager: `kernel/src/object` with `KernelObject` (`Thread`,
  `Process`, `Console`) and an initial table per process;
- handle tables: `libs/handles` with index + generation, per-process limit
  (256), 4 host tests;
- lifetime: an `Arc` per object; closing a handle invalidates that value forever;
- rights: `READ WRITE DUPLICATE TRANSFER WAIT SIGNAL MANAGE`; duplicating only
  reduces; each syscall declares the right it requires.

### Result

Syscalls no longer depend on unsafe global identifiers:
`console_write` requires a console handle with `WRITE` (ABI v2), and
`user.handles` checks from ring 3 the cases of a forged handle, a closed one,
one without rights, escalation and the wrong object.

---

# PHASE 13 — Capabilities

**Status: completed (2026-09-26).**

### Implemented

- capabilities: handles with rights over objects shared via `Arc`
  (Phase 12);
- rights: `READ WRITE DUPLICATE TRANSFER WAIT SIGNAL MANAGE`;
- transfer: `HandleTable::take` (requires `TRANSFER`), consumed by the IPC
  of Phase 14;
- revoke/drop: `object_revoke` (requires `MANAGE`) invalidates the object for
  all of its handles in all processes; `handle_close` discards one;
- limited derivation: duplicating only reduces rights; a revoked object cannot
  be duplicated.

In addition, infrastructure brought forward from Phase 27: `sdk/rust` (`aricode-sys`)
with the syscall wrappers and `user/testprogs` with ring 3 programs in
Rust and assembly linked as static ELFs, loaded by the kernel with
`aricode-elf`.

### Essential test

A process without a capability cannot access the resource even if it knows its
identifier: `user.handles` (`object_id` identifiers do not grant
access; a forged or closed handle fails) and `capabilities.revoke`
(revoking requires `MANAGE`; after revoking, no duplicate grants access).

---

# PHASE 14 — IPC

**Status: completed (2026-09-26), except shared memory.**

### Implemented

- endpoints: kernel object with a bounded queue and wait queues;
- send / receive: `ipc_send` (`WRITE`) and `ipc_receive` (`READ`), messages of
  up to 256 bytes and 4 capabilities;
- reply: through a reply endpoint that the client transfers in the message;
- timeout: in ms, `0` does not block, infinite waits; early wakeup;
- capability transfer: `TRANSFER` mandatory; removed from the sender only
  when the message enters the queue;
- shared memory: pending (will come with `memory_map` and memory objects,
  section 24).

### Benchmark

`test=ipc` measures the round trip with the TSC: ~260,000 cycles per round trip
under QEMU TCG (includes creating the client process). It is a relative reference
until KVM or hardware is available (section 69).

---

# PHASE 15 — Init process

**Status: completed (2026-09-26).**

### First user-space process

`services/init` (AriInit), loaded from `aricodeos/initrd.img` (custom
format in `libs/initrd`, read by AriBoot and handed over in `BootInfo`).

Implemented responsibilities:

- start services: `process_spawn` with the `SPAWNER` capability, which only
  init receives;
- supervise processes: `process_wait` distinguishes a normal exit from death by
  fault (`Killed`);
- manage basic dependencies: pending until real services
  exist (Phase 16 onwards).

Verified by `init.boot` (smoke): it launches `hello`, sees `kernel_read` die,
and checks that processes cannot be created without `MANAGE`.

---

# PHASE 16 — Driver framework

**Status: completed (2026-09-26).**

### Implemented

- driver API: syscalls `irq_wait`, `mmio_map`, `region_len`, `dma_alloc`,
  `dma_phys` in `sdk/abi` and `aricode-sys`;
- MMIO capability: `Mmio` object mapped uncached into the process;
- IRQ capability: `Irq` object over an I/O APIC GSI (ACPI MADT, `libs/acpi`),
  with level-triggered masking and release of the vector;
- DMA buffer API: `Dma` object of zeroed contiguous frames, `MANAGE` on
  `Hardware` to reserve it;
- process restart: AriInit restarts `crashy_driver` after seeing it die.

### Criterion

A driver can fail without causing a kernel panic: `init.boot` (driver
killed and restarted) and `drivers.user_irq_mmio_dma` (IRQ, DMA and MMIO from
ring 3).

---

# PHASE 17 — PCIe enumeration

**Status: completed (2026-09-26).**

### Implemented

- ACPI MCFG (`libs/acpi`);
- ECAM mapped uncached, 1 MiB per bus;
- enumerate: walk from the initial bus following bridges, multi-function
  devices;
- decoded BARs (I/O, 32/64-bit memory, prefetchable) and size
  by probing in the library;
- initial MSI: detection of the MSI and MSI-X capabilities (programming
  comes with virtio, Phase 18).

### Real output on QEMU q35

```text
pci: 00:00.0 8086:29c0 class 06.00 host bridge (0 BARs)
pci: 00:01.0 1234:1111 class 03.00 VGA/display (2 BARs)
pci: 00:02.0 8086:10d3 class 02.00 Ethernet (4 BARs, PM/MSI/PCIe/MSI-X)
pci: 00:1f.0 8086:2918 class 06.01 ISA bridge (0 BARs)
pci: 00:1f.2 8086:2922 class 01.06 SATA (AHCI) (2 BARs, MSI)
pci: 00:1f.3 8086:2930 class 0c.05 SMBus (1 BAR)
```

---

# PHASE 18 — Virtio

**Status: completed (2026-09-26): virtio-blk (Phase 18) and virtio-net (Phase 22) in ring 3 over a common modern PCI transport (`aricode-sys::virtio_pci`).**

### Implemented

- virtio transport: `libs/virtio` (split virtqueue over DMA, discovery
  of modern virtio-pci structures, negotiation through `common_cfg`);
- virtio-blk: `services/virtio-blk`, a ring 3 driver with capabilities
  claimed by `init` (`pci_claim`: BARs probed and MSI-X programmed by
  the kernel), queue 0 in DMA, reads and writes verified
  (`virtio.blk_rw`);
- virtio-net: `services/virtio-net`, driver and network stack in a single service (Phase 22).

### Reason

Get storage and networking quickly in QEMU.

---

# PHASE 19 — Storage service

**Status: completed (2026-09-26), except the cache.**

### Implemented

- block service: the virtio-blk driver serves `BlockServiceProtocol v1`
  (`sdk/proto`) over IPC with direct access to the client's DMA buffer;
- async request queue: the endpoint's bounded queue;
- initial cache: pending (will come with the filesystem once there are metrics);
- partition parsing: GPT with CRC (`libs/gpt`), test disk with an
  AriCodeOS partition (`ARI0`).

---

# PHASE 20 — Basic filesystem

**Status: completed (2026-09-26) with AriFS v0; v1 format with CRC32,
replacement journal and `flush` since stage 2 (2026-09-28, ADR 0014).**

### Implemented

- AriFS v0 (`libs/arifs`, no_std, host tests): superblock, flat table
  of 32 fixed-capacity files (16 sectors), reads and writes by
  offset, no fragmentation or journaling (deliberately simple,
  section 34.2). xtask formats the `ARI0` partition of the test disk
  with sample files;
- file service `fsd` (`services/arifs`) in ring 3 on top of the block
  service: `FileServiceProtocol v1` (`sdk/proto::fs`) with `List`,
  `Open`, `Create`, `Read`, `Write`, `Close` and, since stage 2, `Flush` and
  `Replace`; data travels through the client's DMA buffer, not in the
  message;
- clients in the SDK (`aricode-sys::block`, `aricode-sys::fs`) and test
  program `fs_client`; scenario `filesystem.read_write`;
- permissions through capabilities: a process can only talk to `fsd` if init
  gives it a `WRITE` duplicate of its endpoint; `fsd` only reaches the disk
  through the block service endpoint.

### Pending

- hierarchical directories and paths (v0 is a flat directory);
- metadata (dates, type) and deletion/resizing;
- per-file capabilities (today the capability is per service).

### Criterion

A user-space process can open, read and write a file through an IPC service. **Met.**

---

# PHASE 21 — Shell

**Status: completed (2026-09-26).**

First AriCodeOS shell: `arish` (`services/shell`), a ring 3 process that
init launches when the cmdline contains `shell` (`cargo xtask run --cmdline shell`).

Commands:

```text
help      ps        mem       ls        cat NAME  write NAME TEXT
run PROG  spawn PROG  repeat N CMD
devices   services  uptime    cpus      diag      reboot    shutdown  exit
```

### Implemented

- console input in the kernel: COM1 IRQ (GSI 4, its own vector) with
  a 1 KiB ring and `console_read(console, buf, timeout)`; the console still
  belongs to the kernel and the shell only reads it through a `CONSOLE` handle
  with `READ` that init transfers to it;
- user keyboards (stage 2, ADR 0013): `console_inject` (`MANAGE` on
  the console) and the `console_bridge` service, which init launches supervised in
  `shell` mode without GUI with the virtio-input and USB HID endpoints; the shell
  does not change (`shell.usb_keyboard`, `shell.virtio_keyboard`);
- `SYSTEM` object (kind 11): `system_info` (`READ`: uptime, usable/free
  memory, process list) and `system_power` (`MANAGE`: power off,
  reboot). Init receives it and passes a duplicate to the shell;
- the shell only sees what init gives it: the fsd endpoint (`WRITE`), hardware
  for its DMA buffer, `SYSTEM`, console input (`READ`) and the PCI root
  read-only for `devices`; `services` shows the mask of services that
  init left running and checks that fsd responds;
- `shutdown` terminates QEMU with `qemu_exit` (`ARICODEOS_SHUTDOWN`) or halts the
  CPU; `reboot` pulses the 8042 reset and, failing that, forces a triple fault;
- the scenario runner accepts `stdin` (lines) and `prompt`: it sends each
  line when the serial output shows the prompt. Scenarios `console.input`,
  `shell.commands`, `shell.exit` and `shell.reboot`.

---

# PHASE 22 — Networking

**Status: completed (2026-09-26).**

### Implemented

- `libs/net` (no_std, no `alloc`, host tests): Ethernet II, ARP with
  a cache, IPv4 (checksum, no fragmentation), ICMP echo, UDP with checksum,
  DHCP client (DISCOVER/OFFER/REQUEST/ACK/NAK with retries) and DNS
  (A query, response with compression, and a response builder for
  test servers);
- `netd` (`services/virtio-net`): virtio-net driver (rx/tx queues in DMA,
  MSI-X, `VIRTIO_NET_F_MAC`) and network stack in a single ring 3 thread: DHCP
  at boot, ARP with frames parked until resolved, replies to ICMP
  echo, UDP sockets and DNS resolution. It serves `NetServiceProtocol v1`
  (`sdk/proto::net`): `Info`, `Ping`, `UdpBind`, `UdpSend`, `UdpRecv`,
  `UdpClose`, `Dns`; datagrams go through the client's DMA buffer and
  waits (ping, recv, dns) stay pending with a deadline without blocking the
  service;
- extended driver contract: after the BARs, init passes extras specific to
  each driver (netd receives `SYSTEM` with `READ` to measure time);
- shared virtio transport (`aricode-sys::virtio_pci`): virtio-blk and
  virtio-net no longer duplicate negotiation or queue creation;
- client `aricode-sys::net`, program `net_client`, shell commands `net`,
  `ping` and `dns`;
- QEMU with user networking (slirp) and a test UDP responder on the
  host (echo and DNS with a fixed A record) that starts with every run.

### Criterion

`ping` works inside QEMU (`ping 10.0.2.2` from the shell answers 4 out of
4; `network.stack` requires it over IPC). **Met.**

---

# PHASE 23 — TCP

**Status: completed (2026-09-26).**

TCP lives in `netd` (user service), with the state machine in
`libs/net::tcp` and no `alloc`: a connection with fixed 4 KiB buffers,
active and passive open, MSS 1400, retransmission with an exponential
timer (500 ms → 8 s, 6 retries), orderly close with TIME-WAIT and
RST. `NetServiceProtocol v1` adds `TcpConnect`, `TcpListen`, `TcpAccept`,
`TcpSend`, `TcpRecv` and `TcpClose`; waits stay pending with a deadline.

Tests (all in `libs/net`, over a simulated wire with controlled
losses, plus `network.tcp` in QEMU against an echo server on the host
and a probe that connects to the guest through `hostfwd`):

- handshake: `handshake_data_and_close`;
- retransmission: `retransmits_lost_syn_and_data`, `gives_up_after_too_many_retries`;
- close: `handshake_data_and_close` (FIN in both directions, TIME-WAIT), `reset_closes_immediately`;
- simulated loss: `random_loss_keeps_stream_in_order` (25% loss, 10,000 bytes in order);
- multiple connections: `multiple_independent_connections` and two simultaneous connections in `network.tcp`.

Pending: a sliding window with several in-flight segments retransmitted
individually (today it resends from `snd_una`), window scaling and SACK
options, and RTT estimation.

---

# PHASE 24 — HTTP client

**Status: completed (2026-09-26).**

First real network application: `libs/http` (`GET` requests, parsing of
the status line and headers, bodies by `Content-Length`, `chunked` or
until close; no `alloc`, host tests), `aricode-sys::http::get`
on top of netd's TCP client (resolves names via DNS if the host is not an
IP), the `http_client` program and the shell's `http URL` command. xtask
serves `/hola`, `/chunked` and 404 on `127.0.0.1:17010` for `network.http`.

Goal `http GET -> response received`: **met** (200 with body,
chunked, 404 and closed port detected).

TLS will come later (Phase 31, cryptography).

---

# PHASE 25 — Modern input

**Status: completed (virtio-input 2026-09-27; xHCI and USB HID 2026-09-28).**

### Implemented

- `libs/input` (no_std, host tests): events in evdev format (the same
  one virtio-input uses and that will serve for USB HID) and a US keyboard with
  shift and caps lock that converts key presses into
  characters;
- `inputd` (`services/virtio-input`): ring 3 virtio-input driver on the
  common virtio transport, one process per device (init launches one for
  each `1af4:1052` function); it reads the device name from its
  configuration, keeps the event queue and serves
  `InputServiceProtocol v1` (`sdk/proto::input`: `Info`, `Read` with a deadline);
- client `aricode-sys::input`, program `input_client` and scenario
  `input.virtio`: xtask injects through QMP (`input-send-event`) the line
  `hola` + Enter on the virtio keyboard and a movement and click on the virtio
  mouse; the scenario requires the exact translation;
- xHCI and USB HID (second part): `libs/xhci` (no_std, host tests) with
  registers, TRBs, rings with cycle bit and link, 32- or 64-byte slot and
  endpoint contexts, ERST, SETUP packets and USB descriptors;
  `usbd` (`services/usb-input`) in ring 3 receives the controller (class
  `0c.03`) through the driver contract, resets it, programs the DCBAA,
  scratchpads, command and event rings, enumerates the ports with a
  device (reset, Enable Slot, Address Device, descriptors,
  `SET_CONFIGURATION`, boot `SET_PROTOCOL`, Configure Endpoint) and
  polls the interrupt endpoints; keyboard and mouse boot reports
  are translated into evdev events in `libs/input`
  (`HidBootKeyboard`, `HidBootMouse`) and served through
  `InputServiceProtocol v1` as a single device
  (`USB HID Keyboard+Mouse`); init launches `usb_input` for each USB
  controller and adds it to the input services;
- scenarios `usb.hid` (`test=usb_hid`, QEMU with `qemu-xhci` + `usb-kbd` +
  `usb-mouse` via `usb_hid = true`, events injected through QMP) and
  `usb.hid_boot` (normal boot with xHCI).

### Pending

- HID with report descriptors (report protocol, keyboards with multimedia
  keys, mice with more buttons), USB hubs, hot plugging and unplugging
  (Port Status Change events are acknowledged but new ports are not
  enumerated after boot) and USB storage.
- Let the shell read from the virtio or USB keyboard in addition to the serial
  and PS/2 console (the compositor already receives them in the GUI).

---

# PHASE 26 — Basic GUI

**Status: completed (2026-09-27).**

### Implemented

- framebuffer abstraction: with `gui` on the cmdline the kernel does not paint on the
  screen and hands the GOP framebuffer to init as an MMIO capability
  (`INIT_FRAMEBUFFER`); `system_info(FRAMEBUFFER)` describes geometry and
  format. The kernel console stays on the serial port;
- surfaces: `libs/gfx` (no_std, no alloc, host tests): `0x00RRGGBB`
  canvases over caller buffers with clipping, rectangles, text
  with the console font, cursor and the `Desktop` model (cascading
  windows, z-order, focus, hit testing, composition);
- compositor (`services/compositor`): ring 3 process that owns the
  framebuffer; it composes into a DMA back buffer and blits it while converting the
  format; it reads the input services, orders their events by arrival
  time (inputd timestamps them) and routes keyboard and clicks to the focused window;
- mouse pointer: an arrow drawn by the compositor, moved with the relative
  events of the virtio mouse;
- windows: `WindowProtocol v1` (`sdk/proto::window`): `Create` (the client
  provides the content buffer as DMA), `Present`, `Poll`, `Close`;
  client `aricode-sys::window::Window` with its own canvas;
- events: `Key` (translated character), `Click` (content coordinates,
  button) and `Focus`, with a pending wait per window.

### Criterion

Two isolated graphical applications can draw simultaneously:
**met**. `gui.two_windows` launches `gui_demo A` and `gui_demo B` (separate
processes with their own buffers), checks the pixels of each window in
the composition, clicks on B through QMP and types `h`: only B receives focus,
click and key; A never sees events meant for others.

### Pending

- moving and resizing windows, closing from the title bar;
- damage-region composition (today the whole screen is repainted);
- having the shell and console live in a window (graphical terminal).

---

# PHASE 27 — Application SDK

**Status: completed (2026-09-27).** See `docs/sdk.md`.

Initial Rust SDK, in `sdk/`: `aricode-abi`, `aricode-sys` (syscalls and
service clients), `aricode-proto` (protocols), `aricode-rt` (runtime:
`entry!`, heap, panic, formatting) and `aricode-build` (linking). The runtime no
longer lives in the test programs: services, applications and tests
depend on the same crates. `apps/hello` is the sample application and the
shell can launch programs from the initrd with `run` (it receives a `SPAWNER`).

APIs and where they are:

```text
process      sys::process_spawn_with / process_wait / process_exit
ipc          sys::endpoint_create / ipc_send / ipc_receive
filesystem   sys::fs::FsClient
network      sys::net::NetClient, sys::http::get
window       sys::window::Window (aricode-gfx canvas)
input        sys::input::InputClient, aricode-input::Keyboard
time         sys::time::{uptime_ms, Clock, sleep_ms}
permissions  sys::permissions::{granted, kind_name, is, drop_handle}
```

---

# PHASE 28 — App sandbox

**Status: completed (2026-09-27).**

### Implemented

- manifests: `libs/manifest` (no_std, host tests) parses
  `NAME.manifest` (`name`, `caps`, `memory`); xtask packs it into the
  initrd next to the program and the kernel serves it to init with `initrd_read`
  (`READ` on the `SPAWNER`);
- capability broker: init serves `LaunchProtocol v1` (`sdk/proto::launch`)
  while the shell is alive; the shell no longer has `SPAWNER`, only an endpoint
  to the broker. For each launch the broker reads the manifest, denies the
  capabilities reserved for the system (`spawner`, `pci`, `framebuffer`) and
  hands over, in a fixed order, only what was requested: read-only `SYSTEM`, private
  directory, network, compositor, inputs and, if needed, `HARDWARE`. Without
  a manifest: console and clock;
- user permissions: the policy lives in the broker (`Caps::APP_ALLOWED`);
  `sys::permissions` lets the application see what it received;
- per-app private directory: `fs::Op::Bind` creates in fsd an endpoint bound
  to a prefix (`app/`); through it only the app's own files can be listed, opened
  and created (AriFS allows `/` in names). The same prefix shares the
  endpoint across launches;
- resource limits: a per-process DMA memory quota, set by whoever
  creates it (`spawn_limits` in `process_spawn`) and charged in `dma_alloc`
  (`Error::Limit` when exceeded).

`hello_app` (manifest `system, fs, 128K`) demonstrates all four things in
`sandbox.manifest`: it only sees its `notas` file, it cannot open `hola.txt` and
a 256 KiB reservation fails with `Limit`; a program without a manifest receives
only console and clock.

### Pending

- CPU and handle quotas per manifest; heap memory quota (today the
  heap is static);
- client identity in IPC (so fsd can tell processes apart without one endpoint
  for each);
- a user-editable policy file in AriFS instead of the
  broker's constant.

---

# PHASE 29 — WASM runtime

**Status: completed (2026-09-27).**

### Goal

Run a portable, isolated application. **Met**: `apps/wasm-demo`
is compiled with `rustc` to `wasm32-unknown-unknown` (MVP + `sign-ext` +
`bulk-memory`) and `wasm_runner`, an application with a manifest (`system, fs,
512K`), loads it from its private directory and runs it with `libs/wasm`, our own
`no_std` interpreter (structured control flow, locals and globals,
bounds-checked linear memory, direct and indirect calls,
imports; no floating point or SIMD, with step and depth
limits). The module's linear memory is a DMA buffer of the runner,
bounded by the manifest quota. Scenario `wasm.demo`.

### API

No POSIX is exposed: the module only imports the Capability API under the
`aricode` module (`print(ptr, len)`, `uptime_ms() -> i64`), resolved by name
by the host, which validates every memory access. Anything the host
does not import does not exist for the module.

### Stage 4 (2026-09-29, ADR 0019)

- full validation of the module before instantiating it (`Error::Invalid`),
  the `aricode-wasm` v1 profile documented in `docs/wasm-profile.md` (it is not
  WASI), a per-module manifest (`demo.manifest`, `caps = print, clock,
  files`) that bounds the imports, and `file_read`/`file_write` on the
  runner's private directory; scenarios `wasm.demo`, `wasm.restricted`
  and `wasm.invalid`.

### Pending

- more Capability API functions (networking and windows of the runner itself);
- floating point;
- performance: it is a byte-by-byte interpreter with no precompilation.

---

# PHASE 30 — A/B update

**Status: completed (2026-09-27).**

### Implemented

- system image: an `aricode-initrd` container with `manifest`, `manifest.sig`,
  `kernel.elf` (stripped) and `initrd.img`, preceded in the partition
  by an `ARISLOT` header with its length; xtask builds it and signs it
  with the development key (`image::system_image`);
- signed manifest: `version`, SHA-256 of kernel and initrd; Ed25519 signature
  over the full text; `aricode-ab::Image::verify` only returns the bytes
  if the signature and hashes match;
- A/B slot: GPT partitions `SYSA` and `SYSB` (12 MiB), `UPDATE` (downloaded
  image) and `ARIBOOT` (state); AriBoot finds them through `BlockIo`
  (non-exclusive open), chooses the slot, verifies it and loads the kernel from the
  image; if there is no `ARIBOOT` or nothing verifies, it boots from the ESP
  (recovery). The kernel receives `boot_slot=A|B [boot_pending]` on the
  cmdline and init sees it in `init_arg`;
- boot attempts counter: `BootState` (sector 0 of `ARIBOOT`, with CRC)
  stores active, pending and attempts; AriBoot consumes an attempt before
  jumping to the kernel and init confirms the slot by writing the state through the
  block service once the boot services are working;
- rollback: pending with no attempts left or with a rejected image → go back to the
  active one and clear the pending one (`aricode-ab::{choose, reject}`).

The updater (`apps/updater`, a trusted application with the `block`
capability) reads `UPDATE`, verifies it, copies it to the inactive slot and schedules three
attempts. Scenarios: `update.slot_a` (smoke), `update.pending_boot`,
`update.rollback_exhausted`, `update.reject_bad_signature`,
`update.esp_fallback` and `update.apply`, each with its own disk variant.

### Pending

- automatic reboot after installing and a test of two chained boots
  (today each scenario starts from a disk prepared by xtask);
- update source over the network (today xtask leaves it in `UPDATE`);
- production key outside the repository and key rotation.

---

# PHASE 31 — Cryptography and trust

**Status: completed (2026-09-27); image signing is applied in Phase 30.**

### Implemented

- `libs/crypto` (no_std, no dependencies, tests with the official vectors):
  SHA-256 and SHA-512 (FIPS 180-4), Ed25519 (RFC 8032: field GF(2^255-19) in
  51-bit limbs, extended points, scalars modulo L) and ChaCha20
  (RFC 7539). Ed25519 is not constant-time: the system only verifies;
  signing is the host's job;
- image signing: `cargo xtask sign` and `cargo xtask verify` on
  files; signed update manifests come with Phase 30;
- hash verification: incremental SHA-256 for large images;
- key management: `cargo xtask keygen`; a versioned development pair in
  `keys/dev` (tests only, see its README) and the rule that the production
  seed never enters the repository;
- secure random: ChaCha20 generator in the kernel, served through
  `system_info(RANDOM)` (`sys::random`);
- entropy sources: `rdrand` if the CPU has it, plus TSC and
  boot-time samples on every seeding and every read; the kernel announces
  which source it uses (`rdrand` or `TSC (no rdrand: limited entropy)`).

### Pending

- `rdseed` and external sources (interrupt jitter, virtio-rng);
- constant-time Ed25519 if the system ever signs.

---

# PHASE 32 — SMP

**Status: completed (2026-09-27).**

Although part of SMP can be brought forward, at this stage the following must be consolidated:

- AP discovery;
- startup;
- per-CPU state;
- SMP scheduler;
- TLB shootdown;
- locks;
- CPU affinity.

### Implemented

- AP discovery: the enabled LAPICs in the MADT (`libs/acpi`); the BSP is
  recognized by its own id and is the destination of all device IRQs
  (I/O APIC and MSI-X) and of the serial console;
- startup: a trampoline at physical page `0x8000` (low memory reserved
  by the PMM and checked in the UEFI map) that takes the AP from real mode to
  long mode with boot tables (identity map of the first MiB + the kernel
  half) and jumps to `ap_entry` on the stack of its idle thread;
  INIT-SIPI-SIPI through the LAPIC ICR, a wait of up to 500 ms per CPU and
  a warning if an AP does not respond; each AP loads the final CR3, copies CR0 and
  CR4 from the BSP, installs its tables, the shared IDT, `syscall`, its LAPIC
  (timer with the count calibrated by the BSP) and enters the idle loop;
- per-CPU state: `kernel/src/cpu.rs` with a static `Cpu` per processor
  (GDT, TSS with a #DF stack, current thread, quantum, `syscall` scratch)
  pointed to by `IA32_GS_BASE`; `swapgs` in `syscall`, in the interrupt
  trampoline when coming from ring 3 and when jumping to ring 3;
- SMP scheduler: `aricode-sched` with a shared ready queue and, per
  CPU, a current thread and its own idle; the scheduler lock is held
  during the context switch and released by the resuming thread
  (`force_unlock`), so that no CPU can take a thread before
  its `rsp` has been saved; only the BSP keeps time and wakes up
  sleepers; an idle CPU receives an IPI (`RESCHED`) when a thread
  becomes ready; a `wake` that reaches a thread that is still `Running` stays pending
  and turns its next block into a yield (no lost wakeups);
- TLB shootdown: `vmm::flush_page` invalidates on the local CPU and, through an IPI
  (`TLB_FLUSH`), on the other online CPUs with acknowledgement; it must be called
  with interrupts enabled and no locks held; a `HALT` IPI stops the
  other CPUs on a panic;
- locks: `IrqLock` disables interrupts only while it holds the lock and
  restores them between spins (a waiting CPU keeps servicing IPIs);
  deadlock detection by spin count as before;
- CPU affinity: `set_affinity` in the policy and `spawn_on` in the kernel; a
  thread with affinity is only taken by its CPU (the others skip it in the queue);
- `system_info(CPUS)` and the shell's `cpus` command; exception and panic
  dumps show the CPU;
- tests: `test=smp` (all MADT CPUs online, pinned threads
  running at the same time on different CPUs, free threads migrating,
  a shootdown observed from another CPU with the exact IPI count, a counter
  under a lock contended by all CPUs), scenarios `smp.boot`
  (smoke, `-smp 4`), `smp.selftest`, `smp.single_cpu_selftest`,
  `smp.shell_user`; scenarios declare `cpus = N` and
  `cargo xtask test --cpus N` forces N processors across the whole suite.

### Pending

- Explicit load balancing (there is only a shared queue and optional
  affinity), priorities and `sched_yield`/affinity from user space.
- x2APIC and more than 16 CPUs; hot-plug.
- Device IRQs always go to the BSP; there is no vector distribution or
  migration.
- The heap still uses a global lock; there are no per-CPU caches.

---

# PHASE 33 — IOMMU

**Status: completed (2026-09-27).**

### To implement

- VT-d or AMD equivalent;
- device domains;
- DMA mappings;
- isolation.

### Implemented

- VT-d: `libs/acpi` parses the DMAR (DRHD units with their device
  scopes and `INCLUDE_PCI_ALL`, RMRR reserved regions);
  `libs/iommu` (no_std, host tests) contains registers and bits,
  capabilities, root and context entries, second-level paging with
  3 or 4 levels with a `clflush` hook for non-coherent units, and
  decoding of fault records; `kernel/src/iommu.rs` programs each
  unit (root table, fault interrupt via MSI to the BSP, global invalidation
  of contexts and IOTLB, `TE`) before any driver starts;
- device domains: one domain per process, created on first use; the PCI
  function is assigned to the domain of the process that maps its
  configuration space or a BAR (`mmio_map` of a region with `source_id`); when
  the process dies its context entry is removed and invalidated, and its
  tables return to the PMM;
- DMA mappings: IOVA = physical address, so drivers do not change;
  `dma_alloc` maps the buffer in the domain of whoever reserves it and
  `mmio_map` of someone else's DMA object (client buffers of the block
  service) maps it in the service's domain; when a buffer is released it is
  removed from every domain before its frames are returned; a device's RMRRs
  are identity-mapped when it is assigned;
- isolation: any other access by the device fails in the unit, which
  records it; the kernel announces it (`iommu: DMA fault from bb:dd.f ...`),
  counts it and exposes it with `system_info(IOMMU)`; QEMU's virtio devices use
  `iommu_platform=on` and the transport accepts `ACCESS_PLATFORM`;
- tests: `test=iommu` with the test driver `dma_rogue` (a legitimate
  translated read, a write to foreign memory blocked, the fault
  recorded, memory intact, domain released) and `iommu.enabled` (smoke:
  the whole service stack works translated); without a DMAR everything keeps
  working without translation.

### Pending

- Interrupt remapping (MSIs pass untranslated) and the AMD-Vi variant.
- Selective invalidation (per domain or page) with an invalidation queue;
  today a global invalidation follows every change.
- One domain per device or shared between processes; today it is per process.
- Verification on real hardware (Phase 34): graphics and USB RMRRs.

---

# PHASE 34 — Real hardware

**Status: preparation completed (2026-09-27); first test on the laptop
pending.** See `docs/hardware.md`.

### Prepared without hardware

- `cargo xtask usb-image [--cmdline S]`: GPT image for a USB stick with the ESP
  in our own FAT32 (`libs/fat`, with long names), AriFS, signed slot A and
  A/B state; the same image boots in QEMU without the emulator's FAT disk
  (scenario `usb.image_boot`);
- console without a serial port: i8042 PS/2 keyboard as the kernel's console
  input (set 1 decoder in `libs/input`, scenario
  `console.ps2`) and output through the framebuffer;
- debugging switches `smp=off` and `iommu=off` on the cmdline
  (scenarios `smp.disabled`, `iommu.disabled`);
- IOMMU on real firmware: devices with RMRRs and graphics
  controllers receive an identity domain (RMRR + framebuffer) before
  translation is enabled, so that the screen and the firmware keep
  working;
- boot, flashing and diagnostics guide in `docs/hardware.md`.

### Pending (needs the laptop)

- UEFI boot from USB, RAM/ACPI/framebuffer, PCIe, IOMMU with real RMRRs.
- Real storage drivers (AHCI/NVMe) and xHCI/USB HID (Phase 25).

This is where the old laptop comes in.

It does not need to be formatted during the first phases.

QEMU will be used first.

When AriCodeOS is able to:

- boot via UEFI;
- show logs;
- manage memory;
- handle exceptions;

a bootable USB stick is created.

## Initial policy

**Do not touch the internal disk.**

Boot AriCodeOS from USB.

Only when storage is stable will dedicating the laptop entirely to the system be considered.

---

# 66. Test laptop

Information to gather before working with real hardware:

```text
exact CPU
RAM
GPU
chipset
NVMe/SATA controller
WiFi
Ethernet
USB controller
UEFI version
Secure Boot state
ACPI tables
PCI device list
```

From Linux these can be saved as hardware documentation:

```bash
lscpu
lspci -nn
lsusb
sudo dmidecode
lsblk
sudo dmesg
```

That way we will have a specification of our first physical target hardware.

---

# 67. Hardware strategy

Do not try to support every existing PC.

First define an **AriCodeOS Reference Machine**.

Example:

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

If the chosen laptop meets the requirements, it becomes our first certified real platform.

---

# 68. Compatibility strategy

An important architectural rule:

> Legacy compatibility must live on top of AriCodeOS, never inside its kernel.

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

That way we can keep a modern kernel even when we need to run old software.

---

# 69. Performance

A microkernel architecture can introduce additional IPC.

Therefore AriCodeOS must not assume that microkernel automatically means faster.

The following will be measured:

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

Optimizations will be made with real metrics.

---

# 70. Internal benchmarks

Future repository:

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

Every critical change can be compared against a baseline.

---

# 71. Observability

AriCodeOS must be designed to be able to explain what it is doing.

Future tools:

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

This will be extremely important during development.

---

# 72. Debugging

First tools:

- serial logs;
- QEMU monitor;
- GDB remote debugging;
- ELF symbols;
- panic backtraces.

Conceptual setup:

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

Later phase:

Once reliable storage exists, the system will be able to save crash dumps.

Contents:

- kernel version;
- CPU state;
- process;
- thread;
- stack;
- registers;
- fault;
- last log buffer.

---

# 74. API and ABI

The following must be clearly distinguished:

```text
Kernel internal API
Kernel syscall ABI
Service IPC protocols
Application API
Driver ABI
Boot protocol
```

They do not all need to evolve at the same pace.

During `0.x` they may break compatibility.

Later they will be versioned.

---

# 75. Versioned IPC protocols

Example:

```text
FileServiceProtocol v1
NetworkServiceProtocol v1
WindowServiceProtocol v1
```

Each message must include an identifier/version when necessary.

---

# 76. Versioned boot protocol

Conceptual example:

```rust
const BOOT_PROTOCOL_VERSION: u32 = 1;
```

The kernel must check that it understands the version handed over by the bootloader.

---

# 77. Suggested internal names

```text
AriKernel      -> kernel
AriBoot        -> bootloader
AriInit        -> first process
AriIPC         -> IPC
AriCap         -> capabilities
AriFS          -> filesystem service
AriNet         -> network service
AriGUI         -> compositor / GUI
AriRuntime     -> application runtime
AriSDK         -> SDK
AriPkg         -> future package manager
```

They are not mandatory; they serve as an initial convention.

---

# 78. Future package manager

It will not be an initial priority.

Planned model:

```text
package
├── signed manifest
├── executable / wasm
├── resources
├── requested capabilities
└── version metadata
```

Transactional installation.

Apps will not modify the base system.

---

# 79. Configuration

Avoid a monolithic global registry.

Preference:

```text
system configuration service
user configuration
per-app configuration
```

A human-readable format when useful, and a binary/versioned representation for internal protocols.

---

# 80. Users

It is not a priority of the initial kernel.

When human identity is implemented, it must be kept separate from capabilities.

```text
User identity != automatic authority
```

Identity can take part in policies, but capabilities determine actual access to objects.

---

# 81. Application security

Every application must be considered potentially compromisable.

Goal:

```text
Compromised browser
       |
       X kernel
       X other apps
       X raw disk
       X microphone without permission
       X arbitrary user files
```

The sandbox must prevent a failure in one application from automatically turning into a full system compromise.

---

# 82. Driver security

A driver will likewise be treated as potentially faulty code.

Example:

```text
WiFi Driver crash
      |
      +-- the failure is logged
      +-- device manager revokes capabilities
      +-- process terminates
      +-- driver restarts
      +-- connections recover when possible
```

It must not cause kernel memory corruption.

---

# 83. Design principles that must NOT be broken

1. Small kernel.
2. Secure by default.
3. Isolated drivers.
4. Legacy compatibility outside the kernel.
5. Explicit APIs.
6. Do not introduce legacy without justification.
7. Test before integrating.
8. Every privilege must be explainable.
9. Every `unsafe` must be justifiable.
10. Every important optimization must be measurable.

---

# 84. Legacy policy

Before adding a legacy feature, ask:

```text
do we really need it?
what current hardware uses it?
can it be implemented outside the kernel?
can it be an optional layer?
does it increase the TCB?
does it open a new attack surface?
```

By default:

```text
NO LEGACY
```

until the need is demonstrated.

---

# 85. First practical goal

## AriCodeOS 0.0.1

It must do only this:

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

Nothing more.

That milestone is important because it will prove that there is a fully custom chain from firmware to kernel.

---

# 86. AriCodeOS 0.0.2

**Status: completed (2026-09-26), tag `v0.0.2`.**

Added:

- BootInfo v2 (with cmdline);
- classified memory map;
- logging with levels (serial + framebuffer);
- panic handler and exceptions with diagnostics;
- GDT/TSS;
- IDT with IST for double fault.

It also includes the leveled test infrastructure and the two audit
rounds (`docs/audits/`).

---

# 87. AriCodeOS 0.0.3

**Status: completed (2026-09-26), tag `v0.0.3`.**

Added:

- physical memory allocator (bitmap, `libs/pmm`);
- virtual memory (own tables, W^X, linear physical map, guard page;
  boot protocol v3, ADR 0009);
- kernel heap (`libs/heap`, on-demand growth).

---

# 88. AriCodeOS 0.0.4

**Status: completed (2026-09-26), tag `v0.0.4`.**

Added:

- `IrqLock` for all global state (`libs/sync`);
- APIC (masked PIC, LAPIC, timer calibrated with the PIT, 100 Hz);
- threads (stacks with guard, context switch, idle);
- preemptive round robin scheduler, sleep and wait queues (`libs/sched`).

---

# 89. AriCodeOS 0.0.5

**Status: completed (2026-09-26), tag `v0.0.5`.**

Added:

- ring 3 (GDT with DPL 3 segments, `iretq`, non-fatal user faults);
- syscalls (`syscall`/`sysretq`, ABI v2 in `sdk/abi`);
- processes (main thread, exit and death, release of the address space);
- address spaces (a PML4 per process with the kernel half shared);
- objects and handles with rights (Phase 12 brought forward, ADR 0010).

---

# 90. AriCodeOS 0.0.6

**Status: completed (2026-09-26), tag `v0.0.6`.**

Added:

- kernel objects and handles (Phase 12, in 0.0.5);
- capabilities with revocation and transfer (Phase 13);
- endpoint-based IPC with timeouts and capability transfer (Phase 14);
- minimal SDK (`sdk/rust`) and user programs in Rust (`user/testprogs`).

This version marks the real birth of the microkernel architecture: two
ring 3 processes communicate through an endpoint without either seeing the other
except through the capabilities it has been granted.

---

# 91. AriCodeOS 0.0.7

**Status: completed (2026-09-26), tag `v0.0.7`.**

Added:

- init process (AriInit from the initrd, SPAWNER capability, supervision);
- driver framework (IRQ, MMIO and DMA capabilities; test drivers in ring 3);
- device manager: enumeration and distribution of capabilities remain in the
  kernel (test bench); the user service arrives with virtio;
- PCIe (ECAM through MCFG, enumeration, BARs, capabilities).

---

# 92. AriCodeOS 0.0.8

**Status: completed (2026-09-26), tag `v0.0.8`.**

Added:

- virtio-blk (Phase 18): ring 3 driver with PCI, MSI-X and DMA capabilities;
- storage service (Phase 19): `BlockServiceProtocol v1` over a GPT disk;
- filesystem (Phase 20): AriFS v0 and the `fsd` service over IPC;
- shell (Phase 21): `arish`, IRQ-driven console input, `system_info` and
  `system_power`.

With this version a user can type on the serial console, list and read
files on a virtio disk through two ring 3 services and power off the
machine without any driver living in the kernel.

---

# 93. AriCodeOS 0.0.9

**Status: completed (2026-09-26), tag `v0.0.9`.**

Added:

- virtio-net (Phase 22): ring 3 driver on the common virtio transport;
- IP (Phase 22): IPv4, ARP, ICMP, DHCP, DNS;
- UDP (Phase 22);
- basic TCP (Phase 23): active and passive connections with retransmission and
  orderly close, all in `netd`.

With this version the system obtains an address through DHCP, answers and sends
pings, exchanges UDP and TCP with the host and resolves names, without the
kernel knowing any network protocol.

---

# 94. AriCodeOS 0.1

**Status: completed (2026-09-26), tag `v0.1.0`; hotfix `v0.1.1`
(2026-09-27) with the fixes from the HTTP audit.** Phases 0 to 24: UEFI
boot, memory, ring 3 processes, scheduler, IPC and capabilities, user-space
virtio drivers, block service, AriFS, shell, network
stack with TCP and HTTP client.

First usable experimental version.

Approximate goal:

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

Still not a complete desktop operating system.

## 94.1 AriCodeOS 0.2

**Status: completed (2026-09-27), tag `v0.2.0`.** Phases 25 to 27:

- modern input: virtio-input (keyboard and mouse) with timestamped events
  and keyboard translation in the client;
- basic GUI: a ring 3 compositor that owns the framebuffer, windows with their own
  buffer, cursor, focus and input routing; two isolated
  applications draw at the same time;
- application SDK: common runtime (`aricode-rt`), `aricode-build`, APIs
  for process, IPC, files, network, windows, input, time and permissions,
  documented in `docs/sdk.md`; `apps/hello` and `run` in the shell.

## 94.2 AriCodeOS 0.3

**Status: completed (2026-09-27), tag `v0.3.0`; hotfix `v0.3.1` with the
fixes from the SDK audit.** Phases 28 and 29:

- application sandbox: manifests, capability broker in init,
  per-application private directory in fsd and a per-process DMA memory
  quota; the shell launches through the broker and can no longer create processes;
- WebAssembly runtime: our own `no_std` interpreter and `wasm_runner`, an
  application with a manifest that runs portable modules compiled with
  `rustc`, exposing only the Capability API.

## 94.3 AriCodeOS 0.4

**Status: completed (2026-09-27), tag `v0.4.0`; hotfix `v0.4.1` with the
fixes from audit e9fdf38.** Phases 30 and 31:

- our own cryptography (SHA-256, SHA-512, Ed25519, ChaCha20), the kernel's
  random generator and key tools;
- A/B update: signed system images in slot partitions,
  boot state with attempts and rollback in AriBoot, confirmation by
  init and an updater with the block capability.

Next: 0.5 with SMP (Phase 32) and IOMMU (Phase 33).

## 94.4 AriCodeOS 0.5

**Status: completed (2026-09-27), tag `v0.5.0`; hotfix `v0.5.1` (2026-09-28)
with the fixes from audit 3eb4965: process reaping under SMP,
a single owner per PCI function in the IOMMU and the framebuffer console moved off
the boot stack.** Phases 32 and 33:

- SMP: booting the application processors via INIT-SIPI, per-CPU
  state (`swapgs`), scheduler with a shared queue, per-CPU idle,
  affinity and IPIs, TLB shootdown, console serialized between CPUs and
  `cargo xtask test --cpus N` as a stress run of the whole suite;
- IOMMU: Intel VT-d with one translation domain per driver (IOVA =
  physical), device assignment when its configuration space or a BAR is mapped,
  DMA buffers mapped only in the domains that use them, faults
  recorded and counted; the whole suite runs translated.

Next: Phase 34 (real hardware) and the second part of Phase 25
(xHCI/USB HID).

---

## 94.5 AriCodeOS 0.6

**Status: completed (2026-09-28), tag `v0.6.0`.** Preparation for Phase 34,
the second part of Phase 25 and the fixes from audits 3eb4965 and
4836023:

- USB stick image with our own FAT32 ESP (`cargo xtask usb-image`),
  PS/2 keyboard as console, `smp=off` and `iommu=off` switches,
  identity domains for firmware devices and a hardware guide;
- xHCI and USB HID keyboard/mouse in ring 3 (`libs/xhci`, `usbd`), with
  selection by programming interface and publication of input services
  only once they respond;
- SMP-safe process reaping, a single owner per PCI function in
  the IOMMU, the framebuffer console moved off the boot stack and a
  margin check on that stack.

Next: the post-roadmap sequence proposed in
`docs/audits/2026-09-28-referentes-capacidades-oportunidades.md`
(observable recovery, a useful session with a USB keyboard and persistence) and the
first physical test of Phase 34.

---

## 94.6 AriCodeOS 0.7

**Status: completed (2026-09-29), tag `v0.7.0`.** Stages 1 to 4 of the
post-roadmap sequence (section 94.7), each with its ADR and its
scenarios:

- observable recovery: a service supervisor with queryable state
  (ADR 0011) and IPC calls with a per-request reply endpoint and deadlines
  (ADR 0012);
- useful session: USB and virtio keyboards as the shell console (ADR 0013),
  AriFS v1 with CRC32, an atomic replacement journal and end-to-end
  `flush` (ADR 0014), `write` in the shell and multi-boot scenarios
  on the same disk with power cuts;
- limits and updates: an asynchronous broker with `spawn` and a limit on
  running programs (ADR 0015), read-only shared files per
  manifest (ADR 0016), a full A/B update cycle across boots
  with a rollback that preserves data;
- external applications: signed packages built outside the tree and
  installed in AriFS (ADR 0017), a trusted installer with atomic
  chunked replacement (ADR 0018), a WebAssembly validator and the
  `aricode-wasm` v1 profile with a per-module manifest (ADR 0019).

Next: stage 5 (performance, power and more devices) and the
first physical test of Phase 34.

---

## 94.7 AriCodeOS 0.8

**Status: completed (2026-09-30), tags `v0.8.0` and `v0.8.1`.** Stage 5 without
hardware, and in 0.8.1 the `6ee19fb` audit
(`docs/audits/2026-09-29-services-persistence-packages-wasm-6ee19fb.md`):
chunked commits bounded to their transaction, barriers before
publishing metadata, abandoned transactions with a deadline, `process_kill`
for services that do not respond, import signatures and WASM structural
validation, and `loop` parameters.

- reproducible measurements from ring 3 (`bench`: syscall, IPC and files with
  p50/p95/p99; `docs/performance.md` with the method and the baseline for each
  version), per-CPU idle and the `diag` report;
- optimizations guided by those measurements: `ipc_receive_any` (ADR 0020) and
  fsd without polling (IPC to fsd goes from 10 ms to 46 µs), sector batches
  in AriFS and fsd and one `flush` fewer per replacement (4 KiB reads five
  times faster, replacement four times);
- queryable diagnostics: a microsecond clock based on the TSC, recent kernel
  events with their timestamp and cause (`sysinfo::EVENTS`,
  `diag.events`), and a `sleep_ms` that sleeps at least the requested time.

First physical test of Phase 34 (2026-10-10, laptop with the USB stick
flashed with `cargo xtask usb-image`): AriBoot started, verified slot A and
the screen stopped at the slots line. Cause: opening the GOP exclusively
disconnected the firmware console, so the following traces (and
any later failure) were invisible. Fix in
`fix/f34-first-boot-visibility`: non-exclusive GOP, `memory:` traces and
boot beacons (`Beacon`) that narrow down a hang even if the firmware stops
painting (`docs/hardware.md`). With the traces visible, the second and
third photos place the hang in the first `GetMemoryMap`, right after
reserving pages with a custom memory type: ADR 0021, AriBoot reserves
only `LOADER_DATA`, BootInfo v4 carries `kernel_ranges()` and the loader's
tables, and the kernel re-reserves its own ranges when reclaiming boot
memory. With that, fourth photo: **first complete boot of AriCodeOS on
real hardware** (kernel, PCI, init and `ARICODEOS_KERNEL_READY`). The machine's
xHCI only has MSI: ADR 0022 adds MSI to `pci_claim` (scenario
`shell.usb_keyboard_msi`). Sixth photo: xHCI ready but with no devices
(the ports on these Intel machines start out on the EHCIs). ADR 0023: usbd routes the
ports to the xHCI, requests the handoff from the firmware and serves the USB stick as a
block service (bulk-only, SCSI); without virtio-blk, init mounts fsd and the
shell on top of it (scenario `usb.storage`, with a write that survives a
reboot). Seventh photo: the USB stick is read, but it rejects `SYNCHRONIZE
CACHE` (tolerated since then). Eighth photo (2026-10-10): **interactive
AriCodeOS shell on the laptop**, with AriFS on the USB stick and the
PS/2 keyboard; backspace in the framebuffer console was fixed
next, along with a blinking cursor on the screen console and
shell history with the arrow keys (editing keys travel as
terminal escape sequences from any keyboard). Phase 34 is essentially
complete. Networking: ADR 0024 moves the network stack out of virtio-net
into `ari-net-core` (`Link` trait) and adds `rtl-net` for the laptop's
Realtek, with no emulation in QEMU (library tested on the host, driver
on the laptop). Eleventh photo: **wired networking working on the laptop**
(DHCP, ping to the router, DNS). Power management, Wi-Fi (ath9k and WPA2,
out of scope for now) and EHCI remain.

Next: repeat the physical test with the fixed image and, with the
reference machine, power management (sleep states, battery) and more devices.

---

## 94.8 After the roadmap: subsequent sequence

With all 35 phases implemented, development follows the sequence proposed in
`docs/audits/2026-09-28-referentes-capacidades-oportunidades.md`, each
stage with its branches, ADRs and scenarios:

| Stage | Status | Contents |
|---|---|---|
| 0. Consolidate | completed (0.6.0) | audits resolved with evidence; a single per-capability status table in the README |
| 1. Observable recovery | completed | service supervisor (ADR 0011): states, generations, restart with a budget and growing backoff, endpoint stable across generations, publication only after responding, `status` v1 and `services --detail`; IPC deadlines and correlation (ADR 0012): per-request reply endpoint, bounded waits, no automatic retries of writes (`ipc.late_reply`) |
| 2. Useful session | completed | done: shell with USB and virtio keyboards (console bridge, ADR 0013, `shell.usb_keyboard`, `shell.virtio_keyboard`); AriFS persistence across power cuts (ADR 0014: CRC32, replacement journal, end-to-end `flush`, `filesystem.replace_recovery`, `filesystem.corrupt_detected`); reference machine: the Phase 34 laptop (0.9.0) |
| 3. Limits and updates | completed | done: updates across boots with the same disk and cuts (`[[boots]]`, `kill_on`: `update.full_cycle`, `update.cut_during_install`, `update.rollback_keeps_data`), `write` in the shell (`shell.write_persists`); quotas and saturation (ADR 0015: asynchronous broker, `spawn`, limit on running programs, `limits.hogs`); per-resource permissions (ADR 0016: `read_files` in the manifest, `Share` in fsd, `sandbox.shared_file`) |
| 4. External applications | completed | done: application built outside the tree with the SDK by path, packaged and signed (`cargo xtask app-pack`, `aricode-pkg`), installed in AriFS as a single file and launched by the broker after verifying it (ADR 0017, `apps.external`); trusted installer and transactional update from `pkgs/` with atomic chunked replacement (ADR 0018, `apps.update`, `apps.update_cut`); `aricode-wasm` v1 profile with validator, per-module manifest and files (ADR 0019, `docs/wasm-profile.md`, `wasm.*`). Stage completed; WASI is deliberately left out of the profile |
| 5. Optimization and expansion | in progress: real hardware reached (0.9.0) | done: reproducible measurements (`bench`: syscall, IPC and files with p50/p95/p99, `docs/performance.md`), per-CPU idle (`sysinfo::IDLE`, `cpus`) and the `diag` report (`perf.bench`); first measurement-guided optimization: `ipc_receive_any` and fsd without polling (ADR 0020, `ipc.receive_any`); sector batches in AriFS and fsd and one `flush` fewer per replacement (4 KiB reads five times faster, replacement four times); a µs clock based on the TSC and recent kernel events queryable from `diag` (`diag.events`); on the laptop (0.9.0, ADR 0021 to 0024): boot with standard memory types, MSI, USB stick as a disk over USB, PS/2 keyboard with Spanish layout, cursor and history, wired Realtek networking, `diag` to a file and `fs-cat`; pending: power management (sleep states, battery), Wi-Fi, EHCI |

---

## 94.9 AriCodeOS 0.9

**Status: completed (2026-10-10), tag `v0.9.0`.** First boot of
AriCodeOS on real hardware (Phase 34, Intel 7 series laptop), in eleven
photos and four ADRs:

- AriBoot visible on real firmware (non-exclusive GOP, beacons, memory map
  traces) and standard UEFI memory types with BootInfo v4
  (ADR 0021): the firmware hung in `GetMemoryMap` with custom types;
- MSI in addition to MSI-X for user drivers (ADR 0022), the xHCI DMA buffer
  sized by the scratchpads, firmware handoff and routing of
  the Intel ports to the xHCI;
- the USB stick as the system disk: bulk-only USB storage served
  by usbd as a block service, fsd and the shell on top, optional `SYNCHRONIZE
  CACHE` (ADR 0023, `usb.storage`);
- screen console with backspace, cursor (a kernel thread) and shell
  history with the arrow keys (escape sequences from any keyboard),
  Spanish layout (`kbd=es`), `diag NAME` saved to AriFS and
  `cargo xtask fs-cat` to read it from the host;
- wired networking: shared network stack (`ari-net-core`) and Realtek
  r8169 driver (ADR 0024); DHCP, ping and DNS working on the laptop; link
  state in the network protocol;
- a build-time check of the entry point of every initrd program, and the
  `smp.process_reap` deadline adjusted to the host's
  load.

Next: power management (sleep states, battery), more laptop devices
(EHCI; Wi-Fi out of scope for now) and continuing stage 5.

## 94.10 AriCodeOS 0.10

**Status: completed (2026-10-17), tag `v0.10.0`.** The laptop became a
machine you reach and update over the network, and the project adopted
a failure protocol after its first hard hardware incident. Four ADRs,
three incidents, licensed under Apache-2.0.

- whole project in English (code, messages, documentation, tests); the
  Spanish README is a translation;
- full console line editing (cursor, insert and delete anywhere,
  history) and ACPI power-off (FADT, `\_S5_`), verified on the laptop;
- SSH server (ADR 0025): curve25519-sha256 with strict KEX, ssh-ed25519,
  chacha20-poly1305@openssh.com, public-key login only, closed by
  default without `authorized_keys`, session shells launched by init,
  host key generated on the build host, refused keys logged, idle
  clients dropped after 30 s; tested against the OpenSSH client;
- HTTPS (ADR 0026): TLS 1.3 client with X.509 chain, host name, date
  (CMOS real-time clock) and key-usage checks, 17 embedded roots, the
  crypto it needs (SHA-384, HMAC, HKDF, AES-GCM, ChaCha20-Poly1305, RSA
  and ECDSA verification); tested against RFC 8448 and `openssl
  s_server`, and on real sites from the laptop;
- an external audit of SSH and HTTPS (three findings, all fixed with
  regression tests);
- network updates (ADR 0027): `update` downloads a signed system image
  from a channel over HTTP(S) into the A/B scheme, `cargo xtask
  serve-update` publishes it; verified on the laptop, versions 2 to 5
  installed and booted without the USB stick;
- DHCP probes the offered address with ARP and declines it when another
  host uses it;
- failure protocol (ADR 0028, `docs/FAILURE-PROTOCOL.md`): failures are
  never hidden, `strict` halts the failing component with a state dump
  instead of looping, every anomaly is a fault event in `diag`, doubt
  triggers independent studies before any change, every fix is audited;
- incident 0001, the USB stick: three independent studies and four audit
  rounds rebuilt the storage driver's error handling to the xHCI and
  Bulk-Only Transport specifications (endpoint recovery at every stage,
  no command while another is outstanding, timed-out transfers retired,
  re-enumeration of a device that drops, bounded budgets below the
  clients' waits, REQUEST SENSE after a failed command); measurements on
  the laptop and under Linux then showed the cause: the stick confirms
  some writes seconds after accepting them, so SCSI commands get the
  30 s window SCSI stacks use; the network update that failed every
  time now completes;
- incident 0003: a test run had overwritten the real update channel;
  the scenarios now use their own directory;
- authorship and license: Apache-2.0, NOTICE with the attribution
  requirement, SPDX headers in every source, AUTHORS, notices for AI
  systems.

Open: IPC peer-gone notification (a client of a dead server waits its
full timeout), the `update`/`upgrade` protocol with sources, HID
endpoint recovery, Reset Device for re-enumeration, EHCI, incident 0003
reproduction, the SMP self-test under host load (incident 0002).

## 94.11 AriCodeOS 0.11

**Status: completed (2026-10-17), tag `v0.11.0`.** The first version
signed with the project's release key, published on GitHub and as a
Docker image, and the one that opened the project to other people. Four
ADRs, three incidents closed, one opened.

- update sources with a signed index (ADR 0029): `updater/sources`,
  `index` + `index.sig` (Ed25519) with an anti-replay serial, `update`
  and `upgrade` with a plan and a staged list, the installer in STAGED
  and LIST modes with least privilege, `--signing-key DIR` for a
  production key, a legacy manifest for the transition; verified on the
  laptop, versions 2 to 6 installed over the network;
- IPC peer closed (ADR 0030): a client of a dead server gets
  `PeerClosed` at once instead of waiting its full timeout; endpoint
  pairs, `endpoint_close`, init retires the endpoints of abandoned
  services, sshd sessions end with status 139 when their shell dies;
- EHCI driver (ADR 0031): USB 2.0 root ports, hubs, boot HID keyboards
  and mice, bulk-only storage, the served disk identified by GPT GUID
  and serial; both controllers of the laptop ready at every boot;
- boot self-tests only on request (ADR 0032, incident 0005): the
  test-only programs that wrote to the owner's disk and probed the test
  hosts run only with `selftest`; read-only health checks stay;
- HID endpoint recovery and Reset Device (incident 0001, F10/F14), the
  SMP self-test under host load (incident 0002), the laptop link after a
  network update reproduced on purpose and attributed to the test-built
  image (incident 0003), duplicated sections in `updater/state` with
  repair on read and fault events (incident 0006); incident 0004 (a DMA
  mapping that outlives its buffer) opened and still open;
- kernel base signature, a frozen SHA-512 value in the ABI that the
  shell reports with `l1981`, checked by the ABI tests;
- the HTTP client accepts 8 KiB of response headers (github.com sends
  5.3 KiB);
- Docker: a development and test image that runs the whole suite on any
  machine, and a try image that boots the system in QEMU, published as
  `ghcr.io/aricodelabs/aricodeos`;
- the project opened: CONTRIBUTING, SECURITY, code of conduct, GitHub
  templates (bug, proposal, hardware test report), the official
  repository `AriCodeLabs/AriCodeOS` and the website `www.aricodeos.org`
  in every notice;
- release signing key: generated outside the repository, public half in
  `keys/release/`; the ghcr image is signed with it and the laptop was
  reinstalled from a stick built with it; 0.10.0 stays signed with the
  development key and its release notes say so;
- xtask: every workspace package linted, the `fscat` host test needs no
  built kernel, scenarios with `usb_hid_only_input` and `key_delay_ms`
  so USB keyboards are really exercised.

Open: incident 0004; two xtask runs in the same checkout collide on the
runner's fixed ports and the stripped-ELF directory; an Ethernet link
drop seen once after two hours on the laptop (cable reseat restored it);
EHCI hubs, split transactions and 64-bit addressing only verifiable on
hardware.

## 94.12 AriCodeOS 0.12

**Status: completed (2026-10-20), tag `v0.12.0`.** The version that went
through an external release audit and the failure protocol end to end:
every finding fixed on its own branch with regressions, audited twice by
independent reviewers, verified on the laptop. Five ADRs (0033-0037),
ten incidents closed.

- release audit of 0.11.0 resolved: USB storage drivers reject a CSW that
  did not move exactly 13 bytes and the xHCI driver honours the data
  residue (incidents 0007, 0010); the installer never reads a failed file
  request as an absent file (0008); the updater carries an absolute
  budget through DNS, connect, TLS and every receive (0009, ADR 0033);
- network: refusals named in the shell and recorded as fault events
  (0012); TCP initial sequence numbers and ephemeral ports derived from a
  per-boot secret, RFC 6528 and RFC 6056 (0014, ADR 0034); SYN drops
  recorded and answered with a stateless reset, half-open connections
  capped per listener with a short SYN-ACK budget, TCP table by state in
  `diag` (0013, ADR 0035); every netd client has its own session and its
  sockets are reclaimed when it dies, no client can touch another's
  socket (0016, ADR 0037); a bounded number of frames per loop round;
- file service: every client has its own session and its descriptors and
  abandoned replace are released when it dies (0011, ADR 0036);
- scheduler self-test no longer assumes the wait-queue order on SMP
  (0015);
- HTTP client accepts 8 KiB of response headers; Docker images fixed for
  ARM and for the same-path checkout mount;
- verified on the laptop with version 8 over a production-signed index:
  storage, network, HTTPS, the new `diag` lines, no fault event at boot.

Open: incident 0004 (a DMA mapping that outlives its buffer); the kernel
does not expose its generator's seeding state; EHCI hubs, split
transactions and 64-bit addressing only verifiable on hardware.

## 94.13 AriCodeOS 0.13

**Status: completed (2026-10-22), tag `v0.13.0`.** The version that
closed the last high-priority defect: DMA memory across protection
domains. Each fix reproduced before it was changed, audited by
independent reviewers (three rounds for incident 0004), verified on the
laptop. One ADR (0038), two incidents closed.

- DMA buffer lifetime (incident 0004, ADR 0038): a mapping of a DMA
  buffer keeps the buffer alive, so its frames never return to the PMM
  while any process maps them; new `mmio_unmap` syscall (ABI 5); mapped
  DMA bytes count against the mapping process's quota; quota-less system
  services bounded by an 8-entry buffer cache; freed address ranges
  reused (`libs/vranges`) and partial mappings rolled back;
- driver death: bus mastering is cleared before the dead driver's frames
  are freed, a claim leaves it off until the new driver has reset its
  device, the claim token clears it when dropped, FLR where advertised
  (with Transactions Pending, readiness and `pci.flr=off`), quarantine if
  a device will not stop;
- buffers a device may still own (incident 0017): virtio-blk keeps the
  buffer of a failed request until it has reset the device and recreated
  the queue, and honours `strict`; the USB drivers release such a buffer
  only when the controller confirms the slot was released;
- verified on the laptop with versions 9 and 10 over a production-signed
  index: every driver enables bus mastering itself after its reset, the
  firmware USB handoff works with bus mastering off, no fault event.

Open: the kernel does not expose its generator's seeding state; EHCI
hubs, split transactions and 64-bit addressing only verifiable on
hardware; the driver-death path has not run on the laptop (no driver
died there).

## 94.14 AriCodeOS 0.14

**Status: completed (2026-10-24), tag `v0.14.0`.** The version that took
the system from a LAN to the public internet: updates from the project's
GitHub releases, a development channel that puts a build on the reference
laptop in minutes, and SSH sessions that see what their programs print.
Ten ADRs (0039 to 0048), ten incidents (0018 to 0027), each fix reproduced
before it was changed and every kernel, IPC, network and update change
audited by an independent reviewer.

- Entropy and seeds: `diag` says what seeded the kernel generator (ADR
  0039), and AriBoot carries a seed across boots in `ARIBOOT`, so a machine
  without `rdrand` (the reference laptop) is strongly seeded from its
  second boot (ADR 0043);
- one wait for IPC, interrupt lines and a deadline (`ipc_receive_any`, ABI
  6; ADR 0044): the network info call went from about 10 ms to 70 µs, and
  the xHCI driver lost its 10 ms poll (incident 0024: an event read before
  its cycle bit);
- the compositor repaints only damaged regions at a 16 ms cadence (ADR
  0046);
- updates: a development channel with its own key, build numbers and
  commits, `update rollback` and a stable floor (ADR 0045); images on disk
  verified piece by piece up to the slot size, `apply` verifying the copy
  in the slot (incident 0021); one package limit for the whole system
  (incident 0026); redirects and 16 KiB headers over HTTPS (ADR 0048) and
  GitHub Releases as the public source, with a local mirror of its path
  for tests that need no publishing;
- SSH: programs run from a session write to it through a session console
  (ADR 0047, ABI 7), with flow control in sshd;
- network: a full TCP table recycles the TIME-WAIT slot that ends soonest
  and FIN-WAIT-2 gives up a silent peer after 30 s (incident 0027);
- testing: a parallel test runner with per-run ports, a CI container and a
  host-wide suite lock (ADR 0040), the full suite down from about an hour
  to about twelve minutes for both passes; a thermal guard and container
  limits protect the host;
- verified on the laptop: the carried seed, the wait set, every dev build
  through the dev channel, `run bench` over SSH, installed images verified
  piece by piece (7 MiB images in QEMU),
  and downloads from GitHub through both redirects over TLS.

Proposed, awaiting the owner: agents outside the trusted base (ADR 0041),
the driver licence and trusted vendors (ADR 0042). Open: system image
downloads over 6 MiB (incident 0021), apps over 32 KiB need larger AriFS
files (incident 0026), one SSH command that came back empty and was never
reproduced (incident 0025), EHCI hubs and split transactions only
verifiable on hardware.

## 94.15 AriCodeOS 0.15

**Status: completed (2026-10-27), tag `v0.15.0`.** The version of the
failure-pattern review: a week of public vulnerability news (OpenSSH,
PHP2Shell, Node, Java's TLS, rustls' certificate checks) read not for
their languages but for their methods, and every place AriCodeOS made the
same kind of mistake fixed. 47 findings (docs/audits/2026-10-25-failure-
patterns.md), each on its own branch with an incident, a reproduction
before the fix where one was possible and an independent audit; sixteen
incidents (0028 to 0043) and seven ADRs (0049 to 0055).

- Authority closed on every path: the file service refuses a request
  whose session is gone, sshd keeps its own files (ADR 0050), every
  graphical program gets a compositor session that owns its windows (ADR
  0052), and no application gets the raw keyboard (ADR 0053);
- devices checked, not trusted: the addresses a PCI device reports are
  checked against its BARs and the kernel's memory (ADR 0051), the
  virtqueue keeps its own bookkeeping, xHCI/EHCI offsets are bounded, and
  a driver's device loses a client buffer the driver gave back (incident
  0039);
- error paths that clean up: a failed spawn frees its address space, a
  partial IOMMU map is rolled back, init closes what a failed launch made,
  and the supervisor no longer relaunches a service it gave up on;
- storage and parsers bounded: GPT tables streamed, AriFS layout checked
  and a volume poisoned after a failed commit, the WASM interpreter's fuel
  counting work and imports paying for theirs (ADR 0055);
- network and crypto: TCP closing deadlines, port 22 reserved for sshd,
  random DHCP and DNS identifiers, strict SSH key exchange required,
  constant-time Ed25519 signing, certificate validity checked against a
  release floor without a clock, a TLS seed that never falls back to
  zeros, and a list of distrusted keys refused anywhere in a chain (ADR
  0054) carrying the eight compromised eMudhra intermediates;
- the event ring shared fairly between processes (incident 0040);
- found on the way: the screen console repainted the whole screen per
  line, so test results depended on where the kernel image fell in memory
  (incident 0031), and virtio-blk gave up on requests a busy host disk
  took over 2 s (incident 0043);
- verified: the full suite, 197 scenarios with 1 and with 4 CPUs, and dev
  build 12 on the reference laptop (network, USB keyboard and storage, SSH
  with an unchanged host key, HTTPS to GitHub and Google).

Still the owner's: agents outside the trusted base (ADR 0041), the driver
licence (ADR 0042), downloads over 6 MiB and apps over 32 KiB. Left as
stated by the review: the common-name fallback and wildcards over public
suffixes in X.509, and its informational findings.

## 94.16 AriCodeOS 0.16

**Status: completed (2026-10-27), tag `v0.16.0`.** The version that
closes what the failure-pattern review of 0.15 had left as stated: the
two open parts of its X.509 finding and its thirteen informational
findings, plus two defects and two test races found on the way. Each on
its own branch with an incident, a reproduction first and an independent
audit, re-audited until clean; eight incidents (0044 to 0051) and five
ADRs (0057, 0058, 0060, 0061, 0062).

- X.509: the subject common name is never a host name, an IPv4 host
  matches an iPAddress entry (ADR 0058), and no wildcard stands on a
  public suffix (`*.co.uk`), checked against an extract of the Public
  Suffix List's ICANN section shipped under MPL-2.0 (ADR 0062);
- cryptography: AES without tables (a bitsliced S-box), `ct_eq` behind an
  optimization barrier, Ed25519 verification refusing small-order and
  non-canonical points (1160 forgeries of 12 544 accepted before, none
  after), and keys, handshake secrets and key schedules wiped when they
  end (incident 0044);
- the updater fails closed: a damaged boot state is rebuilt from the slot
  AriBoot booted, a damaged stable floor refuses stable releases until
  the owner consents, and `serve-update` keeps request bytes out of its
  headers (ADR 0060; its first version could have targeted the running
  slot, found by the audit and fixed before the merge);
- sshd: authorized_keys lines with options are refused aloud, by line
  number, and every authentication request but the first `none` counts
  against the limit (incident 0046);
- the kernel: stale handles no longer come back after 65 536 reuses (ADR
  0057), the DMA quota is refunded on every failed allocation, MSI-X
  windows no longer grow with driver restarts, and input typed for a dead
  console reader is discarded (incident 0047);
- PCI: an old claim's interrupt line no longer silences a newer claim, and
  MSI-X is used only where the table is still where it was and the
  function decodes memory (ADR 0061);
- test races fixed, not retried: a port-lease test racing other sockets
  (incident 0049) and `dma_rogue` reading the DMA fault count before the
  fault interrupt was handled with 4 CPUs (incident 0051);
- verified: the full suite, 202 scenarios with 1 and with 4 CPUs, and dev
  build 13 on the reference laptop (HTTPS to GitHub, Google, Wikipedia,
  Cloudflare and the website).

Still the owner's: agents outside the trusted base (ADR 0041), the driver
licence (ADR 0042), downloads over 6 MiB and apps over 32 KiB. Accepted
residuals are listed in each incident's "What remains".

---

# 95. Real hardware: the right moment

There is no need to format the old laptop initially.

Recommended order:

```text
1. QEMU
2. QEMU + UEFI
3. QEMU integration tests
4. USB boot on the laptop
5. test RAM / ACPI / framebuffer
6. test PCIe
7. test storage in read-only mode
8. write to an external USB drive
9. only much later consider the internal disk
```

This minimizes the risk of destroying data or getting stuck during development.

---

# 96. Laptop disk strategy

Initially:

```text
Internal SSD/HDD: DO NOT TOUCH
USB AriCodeOS:     BOOT
```

Later:

```text
USB SSD dedicated to AriCodeOS
```

Finally, if the project matures:

```text
fully dedicated laptop
```

---

# 97. Test environment safety

Before testing real storage drivers:

- back up the laptop;
- disconnect unnecessary disks if possible;
- use a dedicated USB drive;
- implement a read-only mode;
- confirm device IDs;
- add protections against accidental writes.

---

# 98. Definition of Done per phase

Each phase will be considered finished only if it meets:

```text
[ ] implementation
[ ] documentation
[ ] tests
[ ] no unjustified warnings
[ ] automated boot test
[ ] code reviewed
[ ] architecture updated
[ ] ADR if it introduces a relevant decision
[ ] reproducible commit/tag
```

---

# 99. Project management

Adopted model (git flow), detailed in `docs/WORKFLOW.md`:

```text
main        -> always bootable; only tagged merges from develop or hotfix/*
develop     -> integration; must pass cargo xtask test
feature/*   -> development, branches off and returns to develop
fix/*       -> fixes on top of develop
hotfix/*    -> urgent fixes on top of main (also merged back into develop)
experiment/*-> prototypes that may break
```

No direct commits to `main` or `develop`. Merges always with `--no-ff` and
with `cargo xtask check` and `cargo xtask test` green. The first commit
(`v0.0.1`, M0) is the only historical exception.

---

# 100. Research backlog

Topics to study as the project progresses:

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

# 101. Technical risks

## Drivers

Probably one of the biggest jobs in any OS.

Mitigation:

```text
QEMU + virtio first
limited target hardware afterwards
```

## GPU

Modern GPU drivers are extremely complex.

Initial mitigation:

```text
UEFI framebuffer
virtio-gpu
```

## WiFi

Proprietary hardware and firmware can complicate support.

Mitigation:

- Ethernet first;
- adapters with known documentation;
- supportable USB devices later.

## Compatibility

Do not try to solve it while the kernel is being born.

## Scope creep

Avoid building a browser, desktop, package system, AI, etc. before stabilizing the fundamentals.

---

# 102. Project success metric

Early success will NOT be:

> running Chrome.

Early success will be:

```text
AriCodeOS boots
AriCodeOS controls its memory
AriCodeOS recovers from failures
AriCodeOS runs isolated processes
AriCodeOS can deny access
AriCodeOS restarts drivers
AriCodeOS can be tested automatically
```

---

# 103. Independence principle

The long-term goal is to reach the point where AriCodeOS can develop AriCodeOS.

Evolution:

```text
Debian (development workstation)
  |
  v
builds AriCodeOS

AriCodeOS
  |
  v
runtime + toolchain

AriCodeOS
  |
  v
builds a new version of AriCodeOS
```

That moment will represent a **self-hosting** system.

It is not a goal of the first phases.

---

# 104. Future self-hosting

To get there, the following will be needed:

- stable filesystem;
- terminal;
- editor;
- processes;
- mature memory management;
- compiler/toolchain;
- linker;
- build system;
- git or an alternative;
- reliable storage.

It could start with cross-compilation and later port LLVM/Rust or another toolchain.

---

# 105. Relationship with AriCode

The name AriCodeOS fits naturally with the existing AriCode compiler,
but after the assessment in section 7.4 the relationship is defined as follows:

```text
AriCode does NOT take part in the kernel, the bootloader or the TCB.
AriCode is a user workload and, later on, a runtime.
```

## 105.1 Short path: run AriCode binaries without touching the compiler

The binaries produced by `aric` are static ELFs that only use a small set
of Linux syscalls:

```text
read  write  exit  mmap  open  close
socket  bind  listen  accept  epoll_*
clone  futex  wait4
```

A Linux compatibility *personality server* (section 48), running in
user space on AriKernel, can implement that subset and
load static ELFs at `0x400000`. That would allow running the
`aricode_es100m` models and the rest of the AriCode programs on AriCodeOS without
modifying the compiler. It is the first planned integration and fits the
rule in section 68: old things run on top, they never define the kernel.

## 105.2 Long path: native AriCodeOS target

Once the syscall ABI (section 22) and the SDK (Phase 27) are stable,
an `x86_64-aricodeos` target could be added to `aric` that emits the native
application format and uses the Capability API instead of Linux syscalls.
This is work for the AriCode compiler, not the kernel, and it does not block any
phase of the roadmap.

## 105.3 What AriCode brings to the project

- A real, demanding workload (multithreaded int8/int4 inference, SHA-256,
  secp256k1) to validate the scheduler, memory and the compatibility layer.
- A concrete use case for the heterogeneous scheduling design
  (section 19) and for future accelerators.
- A strong reason for the Linux compatibility layer to be designed
  from the start with static ELF and no libc as its first target.

## 105.4 Requirements AriCode places on the roadmap

None before Phase 15. From there on:

```text
Phase 11  user mode         -> needed to load static ELF
Phase 14  IPC               -> needed for the personality server
Phase 20  filesystem        -> open/read of compiled .ari files and weights
Phase 22  network           -> socket/epoll used by aricode-net-http
Phase 32  SMP               -> clone/futex for multithreaded aricode-ml
```

The kernel will not depend on AriCode in any phase.

---

# 106. A possible AriCodeOS philosophy

One possible formulation:

> Modern hardware, minimal privileges, isolated components and no legacy compatibility without a demonstrable reason.

Another:

> Old things may run on top. They must never define the kernel.

---

# 107. Immediate next steps

Recommended order to actually get started:

```text
01. Create the aricodeos repo
02. Create the Rust workspace
03. Create AriBoot UEFI
04. Run it in QEMU/OVMF
05. Create the no_std ELF kernel
06. Load it from AriBoot
07. ExitBootServices
08. Jump to the kernel
09. Serial logger
10. Framebuffer console
11. Panic handler
12. First automated boot test
```

Do not implement yet:

```text
network
filesystem
USB
GPU
WASM
desktop
```

until the basic kernel is stable.

---

# 108. First real working tree

To begin with, the repository can be reduced to:

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

We do not need to create the whole future structure from day one.

This tree is the one that has existed in the repository since M0, extended with
`libs/{elf,x86-paging,x86-cpu,pmm,heap,sync,sched,handles,cmdline,fb-console}` and `sdk/abi` (`no_std` logic with host tests),
`tests/qemu/` (scenarios) and `docs/decisions/0001..0007`.

---

# 109. First acceptance test

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

This test must become the cornerstone of CI.

---

# 110. Final result of the proposed architecture

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

# 111. Conclusion

AriCodeOS must start small.

We do not need a formatted laptop, a graphical interface or hundreds of drivers to say that our own operating system exists.

The first victory will be much simpler:

```text
UEFI
  -> AriBoot
  -> AriKernel
  -> full control of CPU and memory
  -> "AriCodeOS booting..."
```

From there, each new capability must be added in a controlled, isolated, documented and tested way.

The proposed architecture deliberately avoids turning AriCodeOS into a copy of current systems. It adopts modern hardware standards where they are needed, but keeps as its central principles:

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

The old laptop must be reserved as a **physical validation platform**, not as the main development environment. QEMU will be the initial development platform because it allows debugging, automation, snapshots and controlled failures without putting hardware or data at risk.

The long-term goal is to reach a system able to run modern applications, isolate potentially hostile code, restart faulty components and eventually compile new versions of itself.

---

# 112. Next official milestone

## AriCodeOS Milestone M0 — First Boot

**Goal:**

Build a bootable UEFI image that loads our own ELF kernel and produces:

```text
AriCodeOS booting...
ARICODEOS_KERNEL_READY
```

over the serial port and/or framebuffer.

**Deliverables (status 2026-09-26):**

```text
[x] repository                 (git initialized; first commit pending)
[x] Rust workspace             Cargo.toml, rust-toolchain.toml 1.98.1
[x] AriBoot                    boot/  (crate ariboot, x86_64-unknown-uefi)
[x] AriKernel                  kernel/ (crate arikernel, x86_64-unknown-none)
[x] BootInfo v1                boot-protocol/
[x] image builder              cargo xtask image  -> build/esp/
[x] QEMU runner                cargo xtask run
[x] serial logger              COM1 16550, macros print!/println!
[x] framebuffer output         GOP, background + color stripes
[x] panic handler              serial + exit code 35 in test mode
[x] boot integration test      cargo xtask test  -> [PASS] boot.kernel_ready
[x] build README               README.md
```

Result of the first boot in QEMU + OVMF (serial):

```text
AriCodeOS booting...
AriCodeOS Kernel 0.0.1
framebuffer: 1280x800 stride 1280 Bgr @ 0x80000000
kernel: phys 0x1e7bc000 virt 0xffffffff80000000 size 16 KiB
memory map: 117 regions (desc 48 bytes)
ARICODEOS_KERNEL_READY
[PASS] boot.kernel_ready  (qemu_exit=Some(33), 1.5s)
```

**M0 completed.** AriCodeOS is no longer just a design: there is a custom chain firmware -> AriBoot -> AriKernel running on a virtual x86-64 machine.

Technical debt recorded in `docs/architecture.md`: RWX physical identity map until the VMM (Phase 6), firmware GDT/IDT until Phase 4, no framebuffer text console until Phase 3.

**Next milestone:** 0.0.2 (section 86): logging with levels, GDT, IDT, exceptions and double fault stack.
