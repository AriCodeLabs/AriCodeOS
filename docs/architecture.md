# Architecture

The full reference is the master document [../AriCodeOS.md](../AriCodeOS.md).
This file summarizes what has been implemented so far and where each piece lives.

## Status: 0.9.0 (Phases 0 to 34 and post-roadmap stages 1 to 5; first boot on real hardware with USB disk, console and network; power management, Wi-Fi and EHCI pending)

```text
OVMF (UEFI)
  -> EFI/BOOT/BOOTX64.EFI            AriBoot        boot/
       GOP, ACPI RSDP, cmdline.txt, kernel.elf, initrd.img, page
       tables (identity + linear physical map + kernel), BootInfo v4,
       ExitBootServices, jump
  -> aricodeos/kernel.elf            AriKernel      kernel/
       serial + framebuffer console (8x16 font), leveled logger,
       BootInfo report, own GDT/TSS/IDT with IST for #DF,
       exceptions with dump, bitmap PMM, VMM with its own tables
       (W^X, physical map, stack with guard page), heap with on-demand
       growth, masked PIC, LAPIC + 100 Hz timer, interrupts
       enabled, kernel threads, preemptive round-robin scheduler with
       sleep and wait queues, ring 3 processes with their own address space,
       syscalls, handle tables with rights and per-object revocation,
       endpoint IPC with capability transfer and timeouts,
       initrd with init and user programs, init spawns and supervises
       processes with the SPAWNER capability, ACPI + I/O APIC, ring 3 drivers
       with IRQ/MMIO/DMA capabilities, PCIe enumeration via ECAM, init claims
       the virtio-blk (BARs, MSI-X) and launches its ring 3 driver as an IPC
       block service (GPT disk), fsd mounts AriFS v1 on it and serves
       files over IPC; with `shell` on the cmdline init launches arish, which reads
       the serial console and powers off or reboots via `system_power`; netd
       (virtio-net) gets an IP via DHCP and serves ping, UDP and DNS over IPC;
       SMP: the MADT's APs boot via INIT-SIPI and schedule from the
       same ready queue with per-CPU state and IPIs; IOMMU VT-d with one
       domain per driver (IOVA = physical); ARICODEOS_KERNEL_READY
```

## Crates

```text
boot-protocol/       BootInfo ABI (v4): UEFI map, framebuffer, RSDP, kernel, stack, PML4 and tables, BootInfo, cmdline, physical map, preserved ranges
libs/elf/            aricode-elf: minimal ELF64 parser (no_std, host tests)
libs/vranges/        aricode-vranges: virtual address ranges with reuse of freed ones (mmio_map area, ADR 0038; no_std + alloc, host tests)
libs/x86-paging/     aricode-x86-paging: 4-level page tables generic over FrameAlloc/PhysMap (no_std, host tests)
libs/cmdline/        aricode-cmdline: `key` / `key=value` without alloc (no_std, host tests)
libs/fb-console/     aricode-fb-console: text console on a 32 bpp framebuffer, Terminus 8x16 font (OFL), host tests
libs/x86-cpu/        aricode-x86-cpu: GDT/TSS/IDT descriptors and exception table (no_std, host tests)
libs/pmm/            aricode-pmm: bitmap frame allocator over external storage (no_std, host tests)
libs/heap/           aricode-heap: first-fit heap with free list, coalescing and per-allocation header (no_std, host tests)
libs/sync/           aricode-sync: IrqLock/SpinLock with a pluggable interrupt policy, restored between spins; force_unlock (no_std, host tests)
libs/fat/            aricode-fat: minimal FAT32 with long names to build and read the USB image's ESP (no_std + alloc, host tests)
libs/xhci/           aricode-xhci: xHCI and USB: registers, TRBs, rings, contexts, ERST, SETUP and descriptors (no_std, host tests)
libs/usb-storage/    aricode-usb-storage: bulk-only transport (CBW/CSW) and SCSI block CDBs, host tests
libs/rtl8169/        aricode-rtl8169: registers, descriptors and bring-up for the r8169 family over a Regs trait, host tests
libs/iommu/          aricode-iommu: Intel VT-d: registers, capabilities, root/context entries, second-level paging, fault registers (no_std, host tests)
libs/sched/          aricode-sched: round-robin policy over Tid with states, sleep, blocking, exit, multiple CPUs, affinity and pending wakeups (no_std + alloc, host tests)
libs/handles/        aricode-handles: handle table with generations and rights, reduce-only duplication (no_std + alloc, host tests)
libs/initrd/         aricode-initrd: initrd format, zero-copy reader (no_std) and writer (feature `write`, xtask); host tests
libs/acpi/           aricode-acpi: RSDP, XSDT/RSDT, MADT (LAPIC, I/O APIC, overrides), MCFG; no AML (no_std + alloc, host tests)
libs/pci/            aricode-pci: ECAM addressing, configuration header, BARs, capabilities, classes (no_std + alloc, host tests)
libs/virtio/         aricode-virtio: split virtqueue over DMA, modern virtio-pci structures, virtio-blk requests (no_std, host tests)
libs/gpt/            aricode-gpt: CRC32, GPT header and entries, writer for the test disk (no_std, host tests)
libs/ab/             aricode-ab: A/B boot state, signed system images and slot policy (no_std, host tests)
libs/crypto/         aricode-crypto: SHA-256, SHA-512, Ed25519 and ChaCha20 from scratch with official vectors (no_std, host tests)
libs/wasm/           aricode-wasm: WebAssembly MVP interpreter with integers, bounded memory and imports (no_std + alloc, host tests)
libs/manifest/       aricode-manifest: application manifests (name, capabilities, memory) (no_std, host tests)
libs/gfx/            aricode-gfx: 0x00RRGGBB canvases, clipping, text, cursor and window desktop (no_std, host tests)
libs/input/          aricode-input: evdev events and US keyboard (no_std, host tests)
libs/http/           aricode-http: GET, response parsing, Content-Length and chunked without alloc (no_std, host tests)
libs/net/            aricode-net: Ethernet, ARP, IPv4, ICMP, UDP, TCP, DHCP client and DNS without alloc (no_std, host tests)
libs/faults/         aricode-faults: bounded fault reporting, one kernel event per kind per second with the count (netd, fsd; no_std, host tests)
libs/arifs/          aricode-arifs: AriFS v1, flat directory with a fixed-capacity file table, CRC32 and replacement journal over `BlockDevice` (no_std, host tests with power cuts)
libs/pkg/            aricode-pkg: signed application packages (manifest with name, version and hashes; Ed25519 signature; ELF; application manifest)
libs/update-index/   aricode-update-index: update sources, signed index, rollback serials, version and downgrade rules (ADR 0029)
sdk/proto/           aricode-proto: versioned IPC protocols of the services (block v1, file v1, net v1, input v1, window v1, launch v1) (no_std, host tests)
sdk/runtime/         aricode-rt: user program runtime (`entry!`, 256 KiB heap, panic, fmt); re-exports aricode-sys
sdk/build/           aricode-build: one-line `build.rs` that links with `sdk/rust/user.ld`
apps/updater/        updater: signed indexes from the update sources, system image into the inactive slot, packages staged for the installer (`fs`, `net`, `block`; ADR 0029)
apps/wasm-demo/      wasm_demo.wasm: portable module compiled to wasm32 that only uses the imported Capability API
apps/wasm-runner/    wasm_runner: WebAssembly runtime as an application with a manifest (reads the module from its private directory)
apps/hello/          hello_app: SDK example application (console, permissions, time)
sdk/abi/             aricode-abi: syscall numbers, errors, initial handles, rights and object types (no_std, host tests)
sdk/rust/            aricode-sys: syscall wrappers (no_std, no alloc), clients for the block, file and net services, HTTP GET client, virtio PCI transport for drivers, and `user.ld`, the linker script for every user program
user/testprogs/      ring 3 test programs (Rust and assembly) linked as static ELFs at 0x400000
services/init/       AriInit: first process; receives SPAWNER and the PCI root, spawns/supervises services and claims devices for their drivers
services/virtio-blk/ ring 3 virtio-blk driver and IPC block service (modern interface, queue 0 in DMA, MSI-X, GPT)
services/virtio-net/ netd: virtio-net driver and network stack (DHCP, ARP, ICMP, UDP, TCP, DNS) over IPC in ring 3
services/net-core/   ari-net-core: minimal IPv4 stack and NetServiceProtocol server, independent of the NIC (Link trait, ADR 0024)
services/rtl-net/    ring 3 Realtek r8169 driver (RTL8169/8168/8101) on top of ari-net-core; no QEMU emulation
services/compositor/ compositor: owns the framebuffer, windows, cursor and input routing in ring 3
services/virtio-input/ inputd: virtio-input driver (one process per keyboard or mouse) and events over IPC in ring 3
services/arifs/      fsd: AriFS file service in ring 3 on top of the block service
services/shell/      arish: interactive ring 3 shell (help, ps, mem, ls, cat, write, devices, services, uptime, cpus, diag, net, run, spawn, repeat, reboot, shutdown, exit; history with the arrow keys)
boot/                ariboot: integrates the above with the `uefi` crate
kernel/              arikernel: serial, framebuffer (MMIO Surface), console (log::Log with IrqLock and try_lock),
                     arch/x86_64 {gdt, idt, interrupts (stubs.s), exceptions, port, pic, pit, apic, ioapic, msr, syscall, usermode},
                     acpi (table discovery), pci (ECAM, enumeration, claiming with BAR and MSI-X probing),
                     hw (IrqLine by GSI, MSI-X or MSI, Region, DmaBuffer, uncached windows, vector registry, test bench),
                     object (revocable KernelObject, initial table), ipc (Endpoint, Message, benchmark), initrd,
                     user/{process (ELF loader, copy_from/to_user, exit wait), programs (initrd), syscalls},
                     memory/{phys, pmm, vmm, heap}, sync (IRQ policy), time,
                     task/{context (switch_context, thread_start), stack, thread, scheduler, wait}, report, panic, QEMU exit
tools/xtask/         build, image, qemu, scenario, test, check
tests/qemu/          declarative QEMU scenarios (see tests/README.md)
```

Rule: logic that can be tested on the host lives in `libs/*`; `boot/` and
`kernel/` only integrate.

Memory-safety invariants in the loader (audit 2026-09-26,
`docs/audits/`):

- `aricode-elf` rejects any ELF whose virtual range is not representable,
  that has overlapping segments, or whose entry point is not in an executable
  segment, and discards empty `PT_LOAD`s so they take no part in mapping. Nothing
  is allocated or copied until the whole ELF has passed validation.
- `aricode-x86-paging` is generic over two providers: `FrameAlloc` hands out
  4 KiB frames (bump over UEFI pages in the loader, PMM in the kernel) and
  `PhysMap` translates physical addresses to pointers (identity in the loader,
  linear map in the kernel). `PhysMap` is an `unsafe` trait: the implementer guarantees
  the translation is valid for every frame the allocator can hand out. The
  `Mapper` dereferences the tables it created with its `FrameAlloc` and, after
  `share_kernel_half` (also `unsafe`), the kernel tables it copies by
  reference: the caller guarantees that the source PML4 is valid and that its
  PDPTs live as long as the address space sharing them (always, in the
  kernel's case).

## AriBoot -> AriKernel contract

Defined in `boot-protocol/` (`BootInfo`, version 4, ADR 0009 and 0021). On kernel entry:

- interrupts disabled;
- CR3 points to tables built by AriBoot: identity with 2 MiB pages
  (loader only), linear physical map at `PHYS_MAP_BASE` (RW, NX)
  for all non-reserved physical memory (at least 4 GiB) plus the framebuffer,
  and the kernel at `0xFFFF_FFFF_8000_0000` with 4 KiB pages and per-ELF-segment
  permissions;
- `rsp` on a 64 KiB stack allocated by the loader, addressed through the
  physical map;
- `rdi` = pointer to `BootInfo` through the physical map; the addresses it
  contains are physical (`phys_map_offset` translates them);
- EFER.NXE set;
- if `\aricodeos\cmdline.txt` existed, its contents are copied into kernel
  memory and referenced by `cmdline_address/size`.

All memory AriBoot allocates is `LOADER_DATA` (ADR 0021: with custom
types some firmwares hang in `GetMemoryMap`). In the map it is
boot memory (`Boot`); `BootInfo::kernel_ranges()` (image, initial
stack, `BootInfo` page, initrd, cmdline) says what the kernel keeps
and `page_tables_base/size` which tables it reclaims when activating its own.

## Console and logging (Phase 3)

- `print!`/`println!` write unformatted text to serial and framebuffer
  (banner and markers such as `ARICODEOS_KERNEL_READY`).
- The `log` crate macros (`trace!`..`error!`) go through `console::KernelLogger`,
  which prepends `[LEVEL]` and writes to both outputs. Default level `info`;
  `log=<level>` on the cmdline changes it and an invalid value produces a `WARN`.
- The panic handler writes to serial only (`serial_println!`): it touches no
  shared state, in case the panic happens midway through a framebuffer write.
- The framebuffer console keeps a copy of the cells and repaints on
  scroll; it never reads from VRAM. It understands backspace (`\x08`, the cursor
  moves back one cell) and draws a `_` cursor toggled by a kernel thread
  (`console::start_cursor_thread`, every 500 ms, with `try_lock`). Neither the tick
  (nothing is painted from handlers) nor the idle loop: one more call in that
  loop, which spins a lot, slowed the whole system in QEMU by 25 %. It only
  blinks while the console is idle (no writes since the previous
  blink); any write hides it before painting, so the
  cells never contain the cursor.
- Layouts: `aricode_input::Layout` (`Us`, `Es`; the Spanish one with
  `AltGr` for `@ # [ ] { } \ | ~` and without the non-ASCII keys). `kbd=es` on
  the cmdline applies it to the kernel's PS/2 keyboard (scenario
  `console.ps2_es`, with `shift+dot` combinations via QMP); the console
  bridge and the compositor remain on US.
- Editing keys (arrows, home, end, delete) reach the console
  as a serial terminal's escape sequences (`ESC [ A`…, `ESC [ 3 ~`),
  whether they come from the kernel's PS/2 keyboard, the console bridge or the compositor:
  `aricode_input::Keyboard::feed` returns `KeyBytes` (up to 4 bytes).

## CPU (Phase 4)

- Own GDT with the layout required by `syscall`/`sysret`
  (`aricode-x86-cpu::layout`, the single reference): null, ring 0 code (0x08),
  ring 0 data (0x10), user data (0x18, selector 0x1B with RPL 3), user
  code (0x20, selector 0x23), TSS (0x28, 16 bytes). The descriptors are
  encoded in `aricode-x86-cpu` (tested against known values) and
  `arch::x86_64::gdt` loads them, reloads CS with `retfq` and checks the layout.
- TSS with IST1 pointing to a dedicated 16 KiB stack for #DF, so that a
  double fault with a corrupted stack still produces diagnostics.
- IDT with all 256 vectors pointing to generated stubs (ADR 0008). Vectors
  with and without an error code are normalized into the same `InterruptFrame`.
- Exceptions: #BP warns and continues; an exception with `CS.RPL = 3` terminates
  the user process (see User mode); the rest dump registers, plus for
  #PF CR2, access type, mode and cause, all over serial, and end in a panic.
  Vectors >= 32 are dispatched to the APIC (see External interrupts).
- Faults triggered via cmdline: `test=breakpoint|invalid_opcode|page_fault|double_fault`.

## Physical memory (Phase 5)

- `memory::pmm::init` walks the UEFI map: it manages up to the end of the
  last non-reserved region, places the bitmap (1 bit per 4 KiB frame) in
  the lowest free region with room, frees the `Free` regions and
  reserves again the first MiB (frame 0, future SMP trampoline) and the bitmap itself.
- `Boot` regions (loader and boot services data) remain
  reserved at this point; the VMM returns them to the PMM when activating its own
  tables (see Virtual memory).
- `test=pmm` runs the self-test on the real bitmap (uniqueness, ordering,
  double free, aligned contiguity) and requires the statistics to return to the
  initial state.

## Virtual memory (Phase 6)

`memory::vmm::init` builds the final tables with PMM frames and
activates them; from then on the loader's identity map no longer exists:

```text
0xFFFF_8000_0000_0000  linear physical map, 2 MiB, RW NX G (non-reserved RAM + framebuffer)
0xFFFF_FFFF_7FFE_E000  guard page (unmapped)
0xFFFF_FFFF_7FFE_F000  kernel stack, 64 KiB, RW NX G
0xFFFF_FFFF_8000_0000  image: .text R-X, .rodata R--, .data/.bss RW- (all G)
```

- Section boundaries come from linker script symbols; end boundaries
  are rounded up to a page (the next section starts on the next page).
- After activating the tables, `kernel_entry` switches to the new stack and continues
  in `kernel_main`; the loader's stack is left unused.
- The UEFI map's `Boot` regions (loader and boot services, ~42 MiB in
  QEMU) are returned to the PMM at that point.
- A stack overflow touches the guard page: #PF without a valid stack -> #DF
  on the IST stack; the handler recognizes `rsp`/CR2 inside the guard and
  reports it as `STACK OVERFLOW`.
- Triggered faults: `test=vmm` (self-test), `write_rodata`, `exec_data`,
  `stack_overflow`.

## Kernel heap (Phase 7)

```text
0xFFFF_C000_0000_0000  heap, 1 MiB chunks mapped on demand, 64 MiB maximum, RW NX G
```

- `memory::heap` is the `#[global_allocator]`; the `alloc` crate (`Vec`, `Box`,
  `String`, `format!`) is available throughout the kernel. `core::alloc` only
  provides the `Layout` and `GlobalAlloc` interfaces.
- When an allocation does not fit, the allocator maps another chunk with PMM
  frames through the VMM and retries; if the maximum is reached or there are no frames
  it returns null and `try_reserve` fails cleanly (a normal allocation
  panics via the default `handle_alloc_error`). Growth is
  transactional: a partial failure unmaps and returns what was already taken.
- The allocator's address arithmetic is checked: in the upper half
  of the virtual address space a valid `Layout` of tens of TiB would overflow
  `usize` (audit `docs/audits/2026-09-26-audit-9d8c7e5.md`).
- `aricode-heap` stores a 16-byte header per allocation with the real
  block: `dealloc` does not depend on the `Layout` and large alignments do not
  waste memory. Free blocks are coalesced on release.
- `test=heap` exercises collections, growth beyond 4 MiB, controlled
  failure and absence of leaks.

## Synchronization

All global state (framebuffer console, PMM, VMM, heap, IDT,
scheduler, process table, wait queues) lives behind `sync::IrqLock`,
which saves `RFLAGS.IF`, executes `cli` on acquire and restores on release: a
critical section cannot be interrupted or re-entered by a handler. Between
wait spins interrupts return to their previous state (Phase 32): a CPU
waiting for a lock held by another keeps servicing IPIs. A lock held for many
spins means re-entry (one CPU) or deadlock, so `lock` panics
instead of hanging. The console uses `try_lock` and falls back to serial only if it is held
(an exception inside the console itself does not block diagnostics). Each
CPU's own state (GDT, TSS, current thread, quantum) takes no lock:
only its CPU touches it, with interrupts disabled (`cpu::Cpu`).

Actual nestings (an outer lock is held while the inner one is
acquired); any other pair is acquired in sequence, never nested:

```text
VMM -> PMM            vmm::with holds the VMM and the Mapper requests frames from the PMM (PmmFrames)
heap.mapped -> VMM    grow holds the growth lock and maps (and, by the above, reaches the PMM)
heap.mapped -> PMM    grow requests and returns frames
PROCESSES -> SCHED    process::wait queries the thread state with the table held
random.RNG -> CARRIED the seed hashes in the carried kernel seed (ADR 0043); CARRIED is a leaf
WaitQueue.waiters     leaf: released before blocking or waking
heap.heap, console    leaf: never call another subsystem
```

Resulting prohibitions: never enter the VMM with the PMM held (the
mapper might need a table and re-enter the PMM); never take the
process table from inside the scheduler (`reap` releases the
scheduler before freeing a user address space). The scheduler
lock is the exception to the guard pattern: it stays held during the
context switch and is released by the resuming thread with `force_unlock` (or
`thread_first_run` on its first run), because with multiple CPUs the outgoing
thread cannot be on the ready queue before its `rsp` has been
saved. `smp::flush_others` (TLB shootdown) requires interrupts
enabled and no lock held: it waits for the other CPUs, and they may
be waiting for the same lock. The heap growth
lock (`mapped`) is distinct from the allocator lock (`heap`):
an allocation takes the latter, a growth takes the former and then the
latter briefly to add the region.

## External interrupts and time (Phase 8)

- The 8259 is remapped to 0x20/0x28 and fully masked; interrupts
  arrive through the Local APIC, mapped uncached at
  `0xFFFF_D000_0000_0000` and enabled with spurious vector 0xFF.
- The LAPIC timer is calibrated against PIT channel 2 (10 ms) and
  programmed periodic at `time::TICK_HZ` = 100 Hz on vector 0x20; in QEMU
  this yields ~62 700 ticks/ms with divisor 16.
- `time` keeps the tick counter and `uptime_ms`; `time::sleep_ms` delegates
  to the scheduler (see Preemptive scheduler). `test=timer` checks the
  rate against the PIT.
- Vectors >= 32 are dispatched from `exceptions::handle` to
  `apic::handle_interrupt`, which sends EOI (except for the spurious one).

## Kernel threads (Phase 9)

```text
0xFFFF_E000_0000_0000  thread stacks: slot = tid, each a guard page (unmapped) + 32 KiB RW NX G
```

- `aricode-sched::RoundRobin` decides who runs next; the kernel stores per thread
  its `saved_rsp` and its stack. The boot context is adopted as thread
  `main` (tid 0) and idle (tid 1, `hlt` + `yield`) is chosen when the queue
  is empty and is never enqueued.
- `switch_context` (System V) saves rbx, rbp, r12..r15 and rsp; a new thread
  starts through `thread_start` with r12 = entry and r13 = argument, enables
  interrupts and on return enters `exit`.
- The scheduler lock is held across the context switch, with interrupts
  disabled, and released by the thread that resumes (`force_unlock` after
  `switch_context`, or in `thread_first_run` for a new thread): no other
  CPU sees a half-switched thread.
- Finished threads are reaped by the next thread that resumes after a
  switch (`reap`), from the policy's exit queue (`pop_terminated`, never a
  scan of every thread; performance audit 731c398, finding E): the stack
  in use is never freed. Thread guard pages
  are also recognized by the `STACK OVERFLOW` diagnostic.
- `test=threads`: two threads strictly alternate with `yield` and 40 workers
  finish and return their stacks to the PMM.

## Preemptive scheduler (Phase 10)

- The timer handler, after the EOI, calls `scheduler::on_tick`: it wakes
  threads whose tick has arrived, earliest deadline first from an ordered
  sleep list (it looks only at the threads it wakes and the next one), and, if the quantum (2 ticks = 20 ms) has
  expired or someone woke up, preempts through the same switch path as
  `yield`. The preempted thread keeps its interrupt frame on its stack and
  resumes through the handler's `iretq`.
- No kernel lock can be preempted: all of them disable
  interrupts. The scheduler lock is held across the switch and released
  by the resuming thread.
- `scheduler::sleep_ms` sleeps without consuming CPU; `time::sleep_ms` delegates to it.
- `task::wait::WaitQueue` implements blocking with a condition evaluated under its
  lock and `wake_one`/`wake_all`; a wakeup that arrives before blocking is not
  lost because `unblock` fails and the thread re-evaluates.
  `wait_any_until_deadline` waits on several queues (ADR 0044): the thread
  registers on all of them before evaluating the condition, outside their
  locks, so no lock is nested and a waker that takes a queue's lock after
  the change always finds it; it leaves every queue on return.
- `test=sched`: three threads that never yield share the CPU (minimum >= half
  of the maximum), a thread sleeps 50 ms with an observed duration of 5 to 7 ticks
  (50 to 70 ms in total), and two consumers are woken one by one and then
  all at once.
- Idle time (incident 0020): a CPU is idle while its idle thread is its
  running thread. `Cpu::set_current`, called at every switch, closes the
  idle stretch when the idle thread stops running and opens one when it
  starts (`aricode_sched::idle::IdleClock`, host-tested). A switch made
  inside a handler taken from `hlt` (the BSP's `on_tick` waking a sleeper)
  therefore ends the stretch at that switch: the woken thread's time is
  never idle. Interrupts serviced on the idle thread that return to it
  without switching count as idle. The idle loop itself measures nothing.
  The TSC of a switch is read under the CPU's idle lock (a leaf lock).
  `scheduler::idle_sample` (`sysinfo::IDLE`) locks every CPU's idle clock
  in CPU order, reads the TSC once and computes the elapsed cycles and
  every CPU's idle cycles, the open stretch included, at that reading: no
  stretch can end after it, so each CPU's idle is at most the elapsed
  cycles and two samples differ by exactly the idle time between them.
- `test=idle`: the control of the performance audit of 731c398 (finding
  A). A thread pinned to CPU 0 and, with more CPUs, to the last one stays
  runnable 500 ms and sleeps 200 ms: its busy interval is excluded from
  idle on its CPU and from the aggregate, and the sleep counts. With more
  CPUs, an idle CPU taking timer ticks and RESCHED IPIs counts the whole
  window as idle (`scheduler.idle_accounting`, `smp.idle_accounting`).

## User mode (Phase 11)

- GDT with the layout that `syscall`/`sysret` requires (`aricode-x86-cpu::layout`):
  kernel code and data, user data and code (DPL 3), TSS. `STAR`,
  `LSTAR` and `FMASK` are programmed in `arch::x86_64::syscall::init`.
- Each process has its own PML4: the upper half is copied from the kernel (256 PDPTs
  reserved at boot and shared, so mappings the
  kernel adds later, such as MMIO windows, are visible in every address space) and the lower half holds its code at `0x40_0000` (read and
  execute only) and its stack at `0x7F_C000..0x80_0000` (RW, NX). The USER bit is
  propagated along the whole table path.
- The process's main thread is an ordinary kernel thread whose `cr3` is the
  process's; it starts in ring 0 and jumps straight to ring 3 with `iretq`.
  On every context switch the scheduler loads the next thread's CR3
  if it differs and sets its kernel stack in `TSS.rsp0` and in the `syscall` entry.
- `syscall_entry` does `swapgs`, saves the user `rsp` in the
  CPU scratch area (`gs:`), loads the thread's kernel stack, builds a
  `SyscallFrame`, enables interrupts and dispatches in Rust; before
  `sysretq` it disables them and undoes the `swapgs`. Convention in `sdk/abi`: `rax`
  number, `rdi rsi rdx r10 r8 r9` arguments, result in `rax`, negative
  `-4095..=-1` = error. Current syscalls: `process_exit`, `console_write`,
  `thread_yield`, `thread_sleep`, `thread_get_tid`, `abi_version`,
  `handle_close`, `handle_duplicate`, `object_kind`, `object_id`,
  `object_revoke`, `endpoint_create`, `ipc_send`, `ipc_receive`,
  `process_spawn` (with handle transfer to the child), `process_wait`,
  `irq_wait`, `mmio_map`, `mmio_unmap` (ADR 0038), `region_len`, `dma_alloc`, `dma_phys`, `pci_count`,
  `pci_info`, `pci_claim`, `console_read`, `system_info`, `system_power`,
  `initrd_read`, `pci_info_ext`, `console_inject`, `process_spawn_from`,
  `ipc_receive_any`, `endpoint_create_pair`, `endpoint_close` (ADR 0030),
  `process_kill` (`MANAGE` on the process: marks it for
  termination and wakes its thread, which exits on leaving the current
  syscall, on waking from a wait, or on the next tick if it was in
  ring 3; `process_wait` returns `Killed`; audit 6ee19fb, 4).
  The block, file and network services add no syscalls: they are IPC.
- User pointers are validated page by page against the process's
  tables (`copy_from_user`): a kernel pointer returns `Fault` and is never
  dereferenced.
- An exception with `CS.RPL = 3` is not a panic: it is diagnosed over serial and
  the process terminates (`Killed`). A not-present page fault on a
  reserved zero-fill page is not an exception to the program: the page is
  backed and the instruction runs again; with less than 1 MiB of frames
  left the process is killed for `out of memory`, and a system call that
  would write into such a page returns `Limit` without consuming anything
  (ADR 0049). When the thread is reaped, the whole
  user address space is freed, tables included.
- Test programs in `user/testprogs` (a separate crate): binaries in
  assembly (`hello`, `kernel_read`, `kernel_write`, `privileged`,
  `bad_syscall`, `bad_pointer`) and in Rust on top of `aricode-sys`
  (`sleep_yield`, `handles`, `caps`, `ipc_server`, `ipc_client`). `cargo
  xtask` builds them as static ELFs for `x86_64-unknown-none` with
  `sdk/rust/user.ld` (0x400000, R-X, R--, RW- segments) and packs them with
  `init` into `aricodeos/initrd.img`. `process::spawn` loads them with
  `aricode-elf` and maps each segment with its permissions (the whole
  zero-fill pages of a writable segment, `.bss` and the runtime's heap
  arena, are only reserved and backed on first touch, ADR 0049). `_start` receives in
  `rdi` the argument from whoever created the process. They are launched with
  `test=user_<name>`.

## Objects and handles (Phase 12, ADR 0010)

- `object::KernelObject` (`Thread`, `Process`, `Console`, and since ADR
  0047 `SessionConsole`, a console that writes to an endpoint) is referenced via
  `Arc` from the handle tables; processes never see pointers or
  global identifiers that confer access.
- Each process is born with three handles: itself (`MANAGE|READ|DUPLICATE`), its
  thread (`READ|WAIT|DUPLICATE`) and the console (`WRITE|DUPLICATE`; a
  session console when the spawner asked with `CONSOLE_FIRST`); limit of
  256 handles per process.
- `user.handles` checks from ring 3: object type, forged handle
  (`BadHandle`), a duplicate without rights that cannot write (`Denied`),
  escalation on duplicate (`Escalation`), closed handle (`BadHandle`) and use of
  an object of the wrong type.

## Capabilities (Phase 13)

- Per-object revocation: `KernelObject` carries a `revoked` flag; `object_revoke`
  requires `MANAGE`, and from then on every handle to that object, in any
  process, returns `Revoked` (except for closing it). There is no need to locate the
  handles: they share the `Arc`.
- Derivation: `handle_duplicate` only reduces rights (Phase 12); a
  revoked duplicate cannot be duplicated again.
- Transfer: `HandleTable::take` removes a handle with `TRANSFER` to
  move it to another table keeping its rights; it is the primitive the
  IPC (Phase 14) uses to pass capabilities between processes.
- `capabilities.revoke` (Rust program `caps`): derive the console, try to
  revoke it without `MANAGE` (`Denied`), revoke the process itself and see the
  duplicate invalidated, close the revoked one and check that the console is still
  alive.

## IPC (Phase 14)

- `ipc::Endpoint` is a kernel object: a bounded queue (16 messages) of
  `Message` (up to 256 bytes and 4 capabilities), two wait queues (senders
  and receivers) and a send counter. `endpoint_create` gives the creator
  `READ|WRITE|DUPLICATE|TRANSFER|MANAGE`; the usual client/server
  model is: the client creates a reply endpoint and sends in each
  message a duplicate with `WRITE|TRANSFER` (the SDK uses
  `endpoint_create_pair`, which splits the two sides into two handles).
- Peer gone (ADR 0030): the kernel counts the capabilities of each side
  of an endpoint (`object::Cap`: `READ` = receive side, `WRITE` = send
  side) wherever they live: handle tables, messages in flight, lists of
  handles for a new process. A process's handles are released when it is
  reaped. When the last capability of a side goes, the side closes for
  good: with no receiver the queue is discarded and every present and
  future send fails with `PeerClosed`; with no sender a receiver gets the
  queued messages and then `PeerClosed`. The work after the close (discard
  the queue, wake the waiters) runs in `ipc::settle_closed` at the end of
  each syscall and after reaping, never under a lock.
  `object_revoke` and `endpoint_close` (`MANAGE`) close both sides.
- `ipc_send` requires `WRITE` on the endpoint and `TRANSFER` on each handle;
  handles are removed from the sender only once the queue confirms the slot
  (`Endpoint::send_with` builds the message inside the critical section),
  so a timeout loses no capabilities. `ipc_receive` requires `READ`,
  copies the data and the received handles into the process's address space
  (`copy_to_user` validates USER and WRITABLE) and returns `len | (n << 32)`.
- `ipc_receive_any` (syscall 31, ADR 0020) receives from the first of up to 16
  endpoints with a message (all with `READ`, validated before waiting) and
  returns its index in bits 48..63. fsd waits this way on its
  main endpoint and on the granted ones at once, without 10 ms polling.
  Since ABI 6 (ADR 0044) the set may also hold up to 4 interrupt lines
  (`WAIT`): the first ready member in list order wins, and a line's
  accumulated interrupts are taken as `irq_wait` takes them (result
  `count | 1 << 47 | index << 48`). The waiter registers on the receive
  queue of each endpoint and on each line's queue before checking, and
  leaves them all on return (`wait::wait_any_until_deadline`): a send wakes
  only its endpoint's oldest waiter, and a set's waiter that took another
  member passes that wakeup on. The line's count is a latch, so an
  interrupt that fired before the wait returns it at once. netd,
  virtio-input and usb-input end each round this way, with the timeout of
  their next deadline (`sdk/rust/src/wait.rs`).
- Timeouts in ms: `0` does not block, `TIMEOUT_INFINITE` waits forever. The
  waits use `WaitQueue::wait_until_deadline`: the thread sleeps with a deadline and
  an early `wake` (`RoundRobin::wake` takes a sleeper off its list)
  wakes it sooner.
- Bounded sizes and queues are the first defence against resource
  exhaustion (section 52); messages that do not fit yield `TooBig`.
- `test=ipc` launches `ipc_server` and `ipc_client` (Rust programs) with the same
  kernel endpoint (server with `READ|WRITE`, client `WRITE` only):
  1000 round trips with a transferred reply handle, plus receive without
  `READ` (`Denied`), timeout, full queue, failed send without losing the handle,
  oversized message and handle without `TRANSFER`. It measures the round trip with the
  TSC: ~260 000 cycles under QEMU TCG including client creation. The
  number is only a relative reference: TCG is not hardware.

## Initrd and init (Phase 15)

- AriBoot loads `\aricodeos\initrd.img` into pages the kernel keeps and
  hands it over in `BootInfo::initrd_*`. Custom format (`aricode-initrd`): magic,
  entry count and, per file, name and data aligned to 8 bytes; the
  reader validates the whole image when opening it and copies nothing.
  The kernel validates it once at `initrd::init` and keeps an index of
  names (the first entry of a name wins), so launching a program no
  longer parses the image (performance audit 731c398, finding F).
- `cargo xtask image` builds the initrd with `init` and all the binaries in
  `user/testprogs`; the kernel no longer embeds programs and is built without
  environment variables.
- Without `test=` on the cmdline, `kernel_main` launches `init` with a single
  extra handle: the `SPAWNER` object with `MANAGE|DUPLICATE` (handle
  `0x0001_0004`). `process_spawn` requires `MANAGE` on a `SPAWNER`, looks up the
  name in the initrd and returns a process handle with
  `READ|WAIT|MANAGE|DUPLICATE`; `process_wait` requires `WAIT`, blocks on the
  process's exit queue (with an optional deadline) and returns the code or
  `Killed`. A process created this way has only the three initial handles.
- `init.boot` (smoke): AriInit launches `hello` (42), sees `kernel_read` die
  as `Killed`, checks that a SPAWNER duplicate without `MANAGE` cannot
  create processes and that a nonexistent name gives `NotFound`.

## User-space drivers (Phase 16)

- `acpi::init` locates the RSDP from `BootInfo`, validates XSDT/RSDT and parses
  MADT and MCFG with `aricode-acpi` through the physical map. `ioapic::init`
  maps the first I/O APIC uncached at `0xFFFF_D000_0000_1000` and masks
  its 24 entries.
- A driver only gets what its capabilities give it (section 27):
  - `Irq`: a GSI routed to a device vector (`0x30..=0x3F`) with the
    polarity and trigger mode from the MADT. The handler accumulates and wakes; level-triggered
    lines stay masked until `irq_wait` (or an `ipc_receive_any` that lists
    the line, ADR 0044) services them.
    When the last handle is dropped, the line is masked and the vector freed.
  - `Mmio`: a physical device region; `mmio_map` maps it into the process
    (`0x1000_0000` onwards, uncached) and records it as foreign memory,
    which is unmapped without being freed when the address space is
    destroyed or on `mmio_unmap`. The addresses come from a per-process
    `aricode_vranges::VirtRanges`: ranges given back by `mmio_unmap` are
    reused, so map/unmap cycles keep using the same page tables (what the
    tables cost is bounded by the most ranges mapped at once). A mapping is
    all or nothing (`map_range_4k` rolls back a range it could not finish).
  - `Dma`: zeroed contiguous frames (`dma_alloc`, at most `DMA_MAX_LEN`,
    8 MiB, requires `MANAGE` on a `Hardware` object); `dma_phys` gives the
    physical address for the device and `mmio_map` maps it cached. The
    frames go back to the PMM when the last reference goes, and a mapping
    is a reference (ADR 0038, incident 0004): a server that keeps a
    client's buffer mapped after closing its handle keeps the frames
    allocated, so no page table ever points at freed frames.
    `mmio_unmap(virt)` drops a mapping: entries cleared, the range
    invalidated (`vmm::flush_own_user_range`; a process has one thread),
    then the addresses given back and the reference dropped. Mapping the
    same range again returns the existing mapping. With a memory quota, the
    DMA bytes a process keeps mapped count against it. Servers bound what
    they keep: the compositor unmaps a window buffer with its last window;
    the block servers, arifs and net-core cache 8 mappings and unmap the
    oldest (`client_buffer::ClientBuffers`; block servers only need the
    IOMMU entry). Dropping the last mapping of a buffer with no handle left
    is logged (`dropped the last mapping of a DMA buffer it had no handle
    to`). `test=dma_lifetime` checks it. A driver that gave a client
    buffer to a device keeps the buffer's handle (`client_buffer::
    DeviceOwned`) until the device cannot reach it: the request completed
    or was retired, the device was reset, or the slot was released; kept
    for good when nothing will say so (incident 0017, note in ADR 0038).
  - Bus mastering and driver death (ADR 0038): `pci_claim` leaves bus
    mastering off; a driver turns it on with `sys::pci::enable_bus_master`
    after resetting its device (virtio transports, rtl-net, the xHCI and
    EHCI drivers), so a device a dead driver left running cannot write to
    memory reused since. When a process is reaped, every PCI function whose
    claim it held alone stops bus mastering before its mappings, IOMMU
    domain and capabilities (its DMA buffers) are freed; a function that
    still reads back as bus master gets the dead driver's DMA buffers
    quarantined for good. The claim itself (`pci::ClaimToken` in the
    configuration object) stops bus mastering whenever it is dropped,
    whoever held it. After the reap a function that advertises FLR (PCIe
    Device Capabilities or the Advanced Features capability) is reset
    unless it has transactions pending or `pci.flr=off` is on the command
    line; the next `pci_claim` waits out the 100 ms and until the function
    answers, then restores the boot-time BARs, cache line, latency and
    interrupt line and the PCIe Device Control registers. All of it is
    logged; anomalies are fault events (`claim held elsewhere`, `bus master
    stuck on`, `flr skipped: tx pending`, `flr: function not ready`).
    `test=pci_reap` checks the order and the bookkeeping with an `igb` that
    advertises FLR.
- `test=drivers`: `pit_driver` services five PIT IRQ0s (channel 0 at 100 Hz)
  and allocates, maps and verifies a DMA buffer; `fb_driver` receives the
  framebuffer as an MMIO region and paints 64x64 pixels; the kernel checks
  pixel (0,0). Restarting a crashed driver is done by AriInit in `init.boot`
  (`crashy_driver` dies with arg 0 and starts with arg 1).
- A process is inserted into the table before its thread is queued
  (`prepare_for_process` + `start_thread`): with `log=trace` the slow output
  let the thread run before its process existed.

## PCIe (Phase 17)

- `pci::init` takes the first MCFG entry, maps its ECAM uncached at
  `0xFFFF_D100_0000_0000` with 2 MiB pages (1 MiB per bus) and walks the
  buses from the starting one, following PCI-PCI bridges. Each function is read
  with 32-bit accesses and decoded with `aricode-pci`: vendor and device,
  class, BARs (I/O, 32- and 64-bit, prefetchable) and capability list
  (MSI, MSI-X, PCIe, PM, AF); the boot line adds `FLR` for a function that
  can be reset on its own (used when its driver dies, ADR 0038).
- On QEMU q35: host bridge, VGA, e1000e Ethernet (MSI/MSI-X), ISA bridge,
  SATA AHCI (MSI) and SMBus. `pci.enumerate` (smoke) requires them.
- The list is returned at boot; MSI/MSI-X programming and
  BAR assignment to drivers arrive with virtio (Phase 18).

## User-space virtio-blk (Phase 18)

- The PCI root is an object that only `init` receives (`0x0001_0005`,
  `READ|MANAGE`). `pci_count`/`pci_info` enumerate from user space; `pci_claim`
  claims a function: the kernel enables memory and bus master, disables
  INTx, probes the size of each memory BAR, programs MSI-X entry 0
  to a device vector (table mapped in an uncached kernel
  window) or, if the function has no MSI-X, its MSI capability with
  a single message (ADR 0022), and returns nine handles: configuration space
  (MMIO), `Hardware`, IRQ and the six BARs (0 if absent), all with
  `TRANSFER`.
- `process_spawn` transfers handles to the child (they appear from
  `0x0001_0004` in order). `init` claims `1af4:1042`, compacts the handles
  present and passes a mask in `arg` (bit 15 IRQ, bits 0..5 BARs); the
  driver reconstructs each BAR's position from it.
- `services/virtio-blk`: reads the configuration space via MMIO, locates
  the common/notify/device structures with `aricode-virtio`, negotiates
  `VERSION_1`, creates queue 0 in a DMA buffer (`QueueLayout` +
  `Virtqueue`), assigns MSI-X vector 0 to the queue and executes
  single-sector requests (header, data, status), waiting for the IRQ with `irq_wait`. The
  self-test reads sectors from the test disk, writes one and reads it back.
- The runner adds a modern `virtio-blk-pci` with `build/disk.img` (4 MiB, GPT
  with the `ARI0` partition formatted as AriFS; from LBA 4096 each
  sector holds `ARIDISK\0` and its number), regenerated on every boot.
- User programs have a 256 KiB static heap
  (`aricode-heap` behind a spinlock) because the PCI library uses `alloc`.

## Block service (Phase 19)

- `init` creates the service endpoint and passes it to the driver along with the
  device capabilities; clients get a `WRITE`-only duplicate
  and a hardware capability for their DMA buffer (init receives it from the
  kernel at `0x0001_0006`).
- `BlockServiceProtocol v1` protocol (`sdk/proto::block`): 16-byte request
  (`Info`, `Read`, `Write`, LBA, sectors) with two handles, the reply
  endpoint and the client's DMA buffer; 16-byte reply with status
  and value. The driver programs the data descriptor with the physical address of the
  client's buffer: zero copies. Up to 16 sectors per request; the endpoint
  queue is the request queue.
- At startup the driver reads the GPT (`aricode-gpt`, header and table CRC)
  and announces the partitions; the xtask test disk carries a protective
  MBR, a GPT with backup and an `ARI0` partition of type AriCodeOS.
- A request the device does not complete (a 2 s timeout with the
  descriptors still in the ring, an unexpected used entry, or
  `DEVICE_NEEDS_RESET`) answers `Io`; the driver keeps the client's buffer
  handle, resets the device (bounded, virtio 1.2 2.4), negotiates the same
  features, creates queue 0 again on its memory, and only then releases
  the buffer (`blk buf kept`, `blk reset`, `blk buf released`). Three
  resets in a row without a completed request, or a reset that fails, make
  it exit (`blk given up`; the reap clears bus mastering first, ADR 0038)
  for the supervisor to restart; with `strict` the first failed request
  halts it instead, before any reset. It receives `SYSTEM` (its command
  line) only when the command line has `strict` or the test-only
  `blk_fault=LBA`, which drops the doorbell of the first request on
  `LBA`; init then runs `blk_late` (`storage.late_completion`, incident
  0017).
- `blk_client` (ring 3) tests info, partition read, write and
  read-back, out-of-range LBA (`Invalid`) and a request without a buffer
  (`BadHandles`). `storage.block_service` requires it. Like the other
  test clients it runs only with `selftest` on the command line (ADR
  0032); every boot runs only init's read-only health check (Info and
  the GPT read).
- User programs have a 64 KiB stack and the kernel diagnoses
  `USER STACK OVERFLOW` when a fault hits its guard page (the 16 KiB GPT
  table on the stack caused it; it now goes on the program's heap).
- Stack use is measured twice (incident 0018). Statically, `cargo xtask
  check` ends with `stack-check`: from `objdump` of the network drivers
  (release and dev builds) it measures every frame and the deepest chain
  of direct calls from `_start` and fails above half the stack for the
  chain or a quarter for one frame (`tools/xtask/src/stack.rs`,
  `tests/README.md`); indirect calls are not seen. At run time,
  `system_info(STACKS)` gives each live process's high-water mark: the
  kernel finds the lowest stack page whose Accessed bit the CPU set
  (`spawn` zeroes the pages through the kernel's own mapping and nothing
  clears the bit), page granular, indirect calls included; `diag` lists
  it as `user stacks (touched of size, page granular)`. Like `PROCESSES`,
  it is readable by any holder of `READ` on `SYSTEM`, and the depth of
  another process is a weak side channel on what it is doing (ADR 0039,
  note of 2026-10-23); there is no address randomization for it to
  defeat. The static check covers every initrd program with per-binary
  budgets (`stack::EXCEPTIONS`, empty).
- `WaitQueue::wait_until_deadline` keeps interrupts disabled
  from the check until the context switch: a `wake` from an
  MSI-X handler cannot land in the window between releasing the lock and
  going to sleep (previously each request could wait up to the 2 s deadline).

## File system (Phase 20)

- AriFS v1 (`libs/arifs`, ADR 0014): LBA 0 superblock (magic `ARIFS`,
  sectors, next free LBA, CRC32), LBA 1..4 table of 32 entries of
  64 bytes (name, starting LBA, capacity, size, per-entry CRC32), LBA
  5 replace journal header and 6..69 its data, data from LBA
  70 in fixed 64-sector blocks allocated incrementally. No
  fragmentation and no deletion. `open` rejects (`Corrupt`) metadata whose CRC
  does not match and redoes a half-committed replace; `write` writes in
  place (whole old or new sector after a power cut, size after
  the data, durable after `flush`); `replace` is atomic and durable through the
  journal (data + CRC, `flush`, pending header, `flush`, copy and
  size, `flush`, cleanup). All access goes through the
  `BlockDevice` trait (with `flush`), so the library is tested on the host over
  a `Vec`, including a cut at every write of the replace with the cache
  lost or kept, and xtask uses it to format the `ARI0` partition
  of the test disk with `hola.txt` and `README` (and to build the
  `fs-pending-replace` and `fs-corrupt` disks).
- `virtio-blk` negotiates `VIRTIO_BLK_F_FLUSH` and handles `Op::Flush` of the
  block protocol (`T_FLUSH`); without the feature, `Flush` returns
  without doing anything and the driver announces this at startup.
- `fsd` (`services/arifs`) receives from init its endpoint (`READ|WRITE`), the
  block service's (`WRITE`), a hardware capability and `SYSTEM`
  (read, a clock to expire replaces abandoned mid-chunk); `arg` is
  the partition's LBA. It implements `BlockDevice` on top of `aricode-sys::block`
  (batches of up to 8 sectors per request: the library groups the whole
  sectors of each read, write, journal chunk and apply, stage
  5) and serves `FileServiceProtocol v1`
  (`sdk/proto::fs`): 64-byte requests and replies with `List`,
  `Open`, `Create`, `Read`, `Write`, `Close`, `Flush`, `Replace`
  (atomic replace of up to `MAX_IO`) and `ReplaceChunk`/`ReplaceCommit`
  (chunks to the journal and commit: up to 32 KiB with the same guarantee; a
  single in-progress replace per service, `Busy` for the rest, released by
  the commit, an error or closing the descriptor; ADR 0018); data goes through the client's DMA
  buffer (up to 4 KiB per request), which fsd keeps mapped in a cache of 8
  by physical address, unmapping the oldest (ADR 0038). The 16 open descriptors
  (`aricode_arifs::descriptors`, host-tested) belong to the endpoint that
  opened them; a client that dies holding some has them released when
  the kernel reports its session gone (ADR 0036, below).
- `aricode-sys::fs::FsClient` wraps the reply endpoint, the DMA buffer
  and the calls; `fs_client` (ring 3) lists, reads `hola.txt`, creates and writes
  `nuevo.txt`, replaces it in one go, does `flush`, reads back and checks
  `Exists`, `NotFound` and `BadFd`, only with `selftest` (ADR 0032; a
  normal boot only lists). `filesystem.read_write` requires it (and the
  negotiated `flush`); `init.boot` requires the success message;
  `filesystem.replace_recovery` redoes on mount a replace cut after
  the commit and `filesystem.corrupt_detected` rejects a tampered table.
- The test disk's raw area (LBA 4096 onwards, pattern
  `ARIDISK\0`+LBA) lies beyond AriFS's maximum reach (70 + 32 × 64
  sectors) so that `blk_client` and `fsd` do not step on each other.

## Shell and console input (Phase 21)

- `console_input` (kernel): COM1's ISA IRQ 4 is routed through the I/O APIC
  to vector `0x40`, outside the user device range; the handler
  drains the FIFO into a 1 KiB ring and wakes a `WaitQueue`.
  `console_read` (syscall 24) requires `READ` on a `Console` object,
  validates the destination before blocking and returns `Timeout` if the deadline expires.
  Bytes dropped because the ring is full are counted for diagnostics.
- User-space keyboards (stage 2, ADR 0013): `console_inject` (syscall 29)
  writes text into the same ring and requires `MANAGE` on the `Console`;
  only init has it (`0x10008` with `READ|MANAGE|DUPLICATE|TRANSFER`) and
  delegates it to the `console_bridge` service, which in `shell` mode without a compositor
  keeps one read outstanding on each input service (virtio-input, USB HID),
  waits on all their replies at once and
  translates keystrokes with `aricode_input::Keyboard`. It is one more supervised
  resident (`services --detail`); the `shell.usb_keyboard`
  and `shell.virtio_keyboard` scenarios type a session over QMP.
- `System` object (kind 11): `system_info` (syscall 25, `READ`) answers
  `UPTIME` (ms), `MEMORY` (usable and free frames: the PMM distinguishes the
  RAM handed over with `free_range` from addressable frames) and
  `PROCESSES` (32-byte entries with pid, state, code and name);
  `system_power` (syscall 26, `MANAGE`) powers off (`ARICODEOS_SHUTDOWN` and
  QEMU exit with `qemu_exit`, `hlt` without it) or reboots (reset via the
  8042, with a triple fault as fallback). ABI v3.
- Init also receives `SYSTEM` (`0x10007`) and a console input
  (`0x10008`, `READ`); the kernel passes `init_arg::SHELL` in `arg` if the
  cmdline contains `shell`. After the boot services it launches `shell` with
  a file session of its own (`Attach`, ADR 0036), hardware, `SYSTEM`, the console input
  and the read-only PCI root, and `arg` = mask of active services; it waits
  for its exit and logs it.
- `arish` reads lines with echo and backspace, and dispatches commands without
  `alloc` (formatting with `fmt_u64`/`fmt_hex` from the user program
  library). `cat` reads in 1 KiB blocks with `FsClient`. The up and down
  arrows (`ESC [ A` / `ESC [ B`) walk a history of the last eight
  distinct lines, which can be edited before running; all other
  escape sequences are discarded (scenario `shell.history`). `diag
  NAME` captures the report output (console and a 16 KiB buffer) and saves it
  to AriFS; `cargo xtask fs-cat DISK [NAME]` reads it from the host
  (scenario `shell.diag_file`, `fscat` test over the test disk).
  The report includes `net`: `Op::Link` (14) of the network protocol reports the
  physical link state (`LINK_UP`/`LINK_DOWN`/`LINK_UNKNOWN`) even if
  DHCP has not finished, `Op::TcpStats` (15) the TCP table by state (one
  `tcp` line, incident 0013), and the `services` summary names roles
  (block, network, input), not drivers.
- The runner (`qemu::run_headless`) accepts `Input { prompt, lines }`: it reads the
  serial output incrementally and sends line `k` once the prompt has
  appeared `k + 1` times, so tests are deterministic without fixed
  sleeps. `console.input` uses `CONSOLE_INPUT_READY` as the prompt to test
  the kernel without a shell.

## Network (Phase 22)

- `aricode-net` is a library of pure encoders over caller
  buffers (no `alloc`): each layer has `parse`/`write` with length and
  checksum validation, and the state machines (ARP cache, DHCP
  client) are tested on the host step by step. The service only connects the
  library to the driver and to IPC.
- `aricode-sys::virtio_pci` concentrates the modern virtio transport
  (PCI structures, `VERSION_1 | desired` negotiation, DMA queues with
  MSI-X vector 0, `DRIVER_OK`, `NEEDS_RESET`) and the handle contract
  with init; after the BARs come the driver's extras (`extra_handle`).
  virtio-blk was reduced to its request logic.
- `netd` is single-threaded: on each iteration it receives up to
  `aricode_net::rx::FRAMES_PER_ROUND = 32` frames (incident 0013: a flood
  cannot hold the round; frames left in the ring are recorded as `net rx
  budget hit` and the next round follows at once), serves up to 8 IPC
  requests with timeout 0 (from its main endpoint and every client session
  together, `ipc_receive_any`; the endpoint list is rebuilt inside the
  round when a request granted or retired a session), expires deadlines and
  then waits once on every endpoint and the NIC's interrupt line until the
  stack's next deadline (ADR 0044: `ipc_receive_any` with the line in the
  set, through the SDK's `wait::end_of_round`); the wait is skipped when
  frames or requests were left over or a sweep took requests, and a request
  that ends it is served at once. Every receive descriptor counts toward
  the 32, an errored one included (rtl-net reports it as length 0). With
  frames left over netd neither waits nor yields, so under a sustained
  flood it runs at 100% of a CPU and fairness towards other processes
  rests on the scheduler's 20 ms quantum; its own clients still get the 8
  requests and the timers every round. A yield was rejected because the
  16-slot receive ring would drop frames meanwhile. On real hardware a
  flood also shows `net tx busy` (8-slot transmit ring; the resets to
  SYNs beyond a listener's cap are best effort, incident 0013). Operations that wait on the network (ping, `UdpRecv`,
  `Dns`) keep the client's reply endpoint and answer when
  the packet arrives or the deadline expires; frames whose next hop is not
  in the ARP cache are parked until the ARP reply (1 s).
- Stack layout of netd (incident 0018): `Stack::new` returns the stack in
  a `Box` (a const assertion keeps the struct under 8 KiB), the UDP socket
  buffers, the four ARP parking frames (allocated once, reused), the
  receive buffer and every TCP socket (`TcpSock::boxed`, out of line) are
  on the heap. Deepest direct path at the fix: 11,752 bytes (release) and
  15,408 bytes (dev) of virtio-net's 64 KiB, against 53,368 and 69,680
  before; a dev build used to overflow on its first DHCP offer (`run` ->
  `on_frame` -> `broadcast_dhcp`). `network.deep_path` drives the deepest
  path (a full TCP table swept inside a request, a dead client's reset
  toward an address with no ARP entry).
- QEMU's network is user-mode (slirp): DHCP hands out 10.0.2.15,
  router 10.0.2.2 (which answers ICMP and redirects UDP to the host's
  loopback) and DNS 10.0.2.3. xtask starts, with each QEMU, a UDP responder on
  `127.0.0.1:17007` that echoes and answers DNS queries with `10.1.2.3`,
  so `network.stack` and the shell do not depend on the Internet. When a
  `netdev` is declared, QEMU stops adding the default e1000: the disk moves to
  `00:02.0` and the network to `00:03.0` (7 functions).

## TCP (Phase 23)

- `aricode-net::tcp::Connection` is a pure state machine: `poll`
  produces at most one segment per call (expired retransmission, SYN,
  new data within the peer's window, FIN, pure ACK) and
  `on_segment` consumes one; the caller transmits and keeps time. Fixed
  4 KiB buffers per direction, retransmission from `snd_una` with exponential
  RTO (500 ms to 8 s, `MAX_RETRIES = 6` → `CloseReason::Timeout`, about
  31 s; a passive connection's SYN|ACK has the short budget
  `SYN_ACK_MAX_RETRIES = 3`, about 7.5 s, so a SYN nobody completes frees
  its slot soon: ADR 0035), 2 s TIME-WAIT, received RST or `abort` →
  `CloseReason::Reset`. The host tests connect two `Connection`s through a
  wire that drops chosen or random segments and pin both schedules.
- In `netd` each TCP socket (`Box`, 8 KiB of buffers) lives in a slot of
  the table; segments are demultiplexed by 4-tuple and a SYN to a
  listening port creates a passive connection that `TcpAccept` delivers once
  established. `TcpConnect`/`TcpAccept`/`TcpRecv` stay pending with a
  deadline; `TcpSend` queues what fits and returns the count (the client
  retries with `tcp_send_all`). A slot is freed when the user has
  closed and the connection has finished; a full table first gives up the
  closed TIME-WAIT connection that ends soonest (incident 0027), and
  FIN-WAIT-2 gives up a peer that never closes after 30 s with a reset.
- Listeners are protected (incident 0013, ADR 0035). A listener holds at
  most `tcp::listen_backlog(MAX_TCP) = 4` connections its owner has not
  accepted yet, half-open and established alike, so a burst of SYNs to port
  22 leaves at least three slots to active opens. A SYN that finds the
  backlog at its cap or no free slot is answered with a reset
  (`tcp::reset_for`, RFC 793 section 3.4: sequence 0, acknowledging the
  SYN) and recorded as the refusal `net syn dropped` (code 5), so the peer
  stops retransmitting and `diag` shows it; a SYN to a port nobody listens
  on is still ignored silently (closed by default). Closing a listener
  resets and frees the connections of its backlog. `Op::TcpStats` returns
  the table by state and `net` (so `diag`) prints it in one line, `tcp
  8 slots: 1 listen, 0 syn-rcvd, 0 syn-sent, 3 backlog, 1 established, 0
  closing, 0 time-wait, 3 free; 4 SYNs reset`: `backlog` are established
  connections nobody accepted, `syn-rcvd` the half-open ones (both count
  towards the cap), `SYNs reset` the refusals since boot. Scenario
  `network.listen_backlog` holds eight connections from the host against
  sshd (QEMU completes the handshakes, so they are backlog, not half-open)
  while the shell's `http` succeeds and `diag` shows the refusal and the
  table.
- Initial sequence numbers and ephemeral ports are secrets (ADR 0034,
  incident 0014). `netd` draws a 32-byte secret per boot from the kernel
  generator (`sys::random` through the `SYSTEM` handle it already holds as
  its clock: `READ | TRANSFER` from init, `READ` is what the kernel
  checks); if the syscall is refused it records the fault event `net tcp secret
  degraded` (code = bytes obtained) and says so on the console instead of
  falling back silently. The kernel always answers `RANDOM` with the bytes
  asked, so that fault cannot mean weak entropy; the quality is read
  apart from `system_info(RANDOM_SOURCE)` (ADR 0039): `tcp secret from
  kernel entropy (rdrand)`, or `tcp secret weak: kernel generator seeded
  from TSC + interrupts (N samples, M events) ...` with the fault event
  `net tcp secret weak x1` (code 2) on CPUs without `rdrand` (QEMU's
  `qemu64`). `aricode_net::isn::initial_seq` gives every connection
  `M + F(4-tuple, secret)` as RFC 6528 prescribes (`M` the ms clock in
  4 µs ticks, `F` the first 32 bits of HMAC-SHA-256): the active SYN and
  the passive SYN|ACK alike, through `Connection::connect(iss, now)` and
  `Connection::listen(iss)`; the connection never derives an ISN from the
  clock or the peer's number. `isn::ephemeral_port` walks the IANA range
  49152..65535 from a per-destination keyed offset plus a connection
  counter (RFC 6056 section 3.3.3) and `isn::pick_ephemeral_port` skips the
  candidates the caller marks in use: `netd` marks a port held by any
  slot, listeners and TIME-WAIT included. Host tests in `libs/net` pin
  each property against the old formulas.
- QEMU forwards a host port chosen per run (ADR 0040) to guest port 7 (`hostfwd`) and
  the xtask responder listens for TCP echo on `127.0.0.1:17008`. A
  scenario can declare `[host_tcp] marker/payload`: on seeing the marker
  in the serial output, xtask connects to the forwarded port, sends the payload and
  appends to the capture a `[host] TCP probe: ...` line that `serial_contains`
  can require.
- The user programs' static heap grows to 256 KiB (netd's TCP
  sockets do not fit in the 64 KiB stack).
- Refusals are visible (incident 0012). netd records a fault event
  (`sys::event_record`, ADR 0028) whenever it refuses a request or loses
  a frame, named by cause: `net tcp table full` (`TcpConnect`/`TcpListen`
  with the 8 slots taken and none in TIME-WAIT to recycle), `net tcp tw
  recycled` (a TIME-WAIT slot given to a new connection, incident 0027), `net arp defer full` (the
  4-slot parking table for frames awaiting ARP), `net tx busy` (the NIC
  had no transmit slot; the frame is lost), `net arp no reply` (a parked
  frame dropped after 1 s), `net ping table full`, `net dns table full`,
  `net udp table full`, `net wait pending` (a second `recv`/`accept` on a
  socket that already waits), `net bad request` (`Status::Invalid`), `net
  bad handles` (`Status::BadHandles`), `net syn dropped` (a SYN to a
  listener refused with a reset: no free slot or the listener's backlog at
  its cap, ADR 0035), `net sessions full` (an `Attach` with every session
  slot taken), `net session create` (an `Attach` whose endpoint pair the
  kernel could not create), `net socks reclaimed` (sockets closed on behalf of a dead
  client, code 0, ADR 0037) and `net rx budget hit` (a round of reception
  that spent its 32 frames with more in the ring, code 0). `aricode_faults::Meter` (`libs/faults`, shared with fsd)
  bounds the rate: at most one event per kind per second; the refusals
  held back in between are counted and recorded when their second ends
  (`drain_due` from `expire`). The event name is `<kind> x<count>` (count
  in that second, shown up to 999; the longest kind, 19 bytes, plus
  ` x999` fills the 24-byte name, checked at compile time) and the event
  code is the `net::Status` number the client got, as every other
  producer uses the code for a reason number: 5 = busy, 1 = invalid, 4 =
  bad handles; a lost frame carries the status its sender would have got
  (`fault reported by process N: net tcp table full x3 (code 5)` = three
  refusals, answered Busy). The same text goes to the console
  (`virtio-net: fault net tcp table full x3`). `BadSock`, `InUse`,
  `NoRoute`, `NotFound`, `Timeout` and `Reset` describe the client's
  socket or the network's answer, not a refusal, and are not recorded. On
  the client side `NetError::describe` (`aricode-sys`) gives every status
  its own line (`network service refused: busy (no free connection or
  send slot)`, `connection refused or reset by the other end`, ...), which
  the shell prints; scenario `network.refusal_visible` fills the table
  with `tcp_hog` and checks the shell line and the `diag` event.

## HTTP client (Phase 24)

- `aricode-http` does not touch the network: `fetch` performs the whole download over a
  `Transport` trait (`send_all`, `recv` with `Ok(0)` = clean close), so
  the crate is tested on the host with a mock transport and the SDK
  (`aricode-sys::http::get`) only resolves, connects and adapts `NetClient`.
  `parse_response` collects all headers before deciding the framing
  (`Transfer-Encoding: chunked` takes precedence over `Content-Length`; other
  encodings or contradictory lengths are invalid),
  1xx responses are consumed before the final one, `ChunkDecoder` is an
  incremental state machine (chunks larger than the scratch buffer, line boundaries
  across reads, sizes validated with no unchecked arithmetic)
  and a close-delimited body only ends with `Ok(0)`: a transport
  reset is an error, never a partial success. The body goes to a caller
  buffer (4 KiB in the shell). Every fetch carries a `Budget` for the
  whole exchange, asked before each send and receive (`Deadline(Phase)`),
  with at most 8 interim responses and 16 KiB of header bytes per fetch
  (incident 0009, ADR 0033). The xtask responder adds a test HTTP
  server on `127.0.0.1:17010` with the `network.http` cases and the
  `/early-hints` routes of `shell.http_deadline`.
- Identifiers are not truncated: `fs::Request::with_name` returns
  `None` if the name does not fit, and the DNS name travels whole in the client's DMA
  buffer (up to 253 bytes) instead of in the IPC message.

## Input (Phase 25)

- virtio-input delivers evdev events (`type`, `code`, `value`, 8 bytes) through
  its queue 0; `inputd` posts 64 8-byte buffers in DMA, replenishes each
  one as it is consumed and stores the events in a 128-entry ring (the
  oldest are dropped if nobody reads). The device name is read from
  `virtio_input_config` (select `ID_NAME`). Init launches one `inputd` per
  `1af4:1052` function with its own endpoint and `SYSTEM` as an extra.
- `InputServiceProtocol v1`: `Info` (name) and `Read(timeout)` with up to
  4 events per reply; a single pending read per service
  (`Busy` if there is another), waiting at most `MAX_READ_MS` (10 s).
  The drivers share `sys::input::EventQueue`: events leave it only once a
  reply carrying them was delivered, so a reader that died with its read
  waiting loses nobody's keys. `aricode-input::Keyboard` translates in the client,
  not in the driver: the driver does not decide the keyboard layout.
- `inputd` and usbd collect the device's events, serve up to 8 requests
  and then wait once on their endpoints and the device's interrupt line
  until the pending read's deadline (ADR 0044); usbd's deadline also
  covers its recovery steps, retried at least 10 ms apart while one waits
  for a command completion. An idle input driver sleeps until a request
  or an event instead of waking every 10 ms.
- QEMU carries `virtio-keyboard-pci` and `virtio-mouse-pci` (functions
  `00:04.0` and `00:05.0`; nine PCI functions in total) and in
  headless runs exposes QMP over a Unix socket in the
  scenario directory. `[host_input]` in a scenario makes xtask, on seeing the
  marker, send `input-send-event` (keys by qcode name, relative
  motion and click); without `device`, because QMP only accepts displays there and
  the virtio devices, registered after the i8042, are the
  active handlers. The result is logged as `[host] QMP: ...`.
- Several boots on the same disk (`[[boots]]`, stage 3): each
  boot has its own `cmdline`, its console lines, optionally
  `kill_on` (xtask kills QEMU on seeing that text: a power cut) and its
  `expect` (`killed = true` when the cut is expected). The test
  disk is built once and kept; each boot has its own ESP and
  its `serial-N.log`. This is how `update.full_cycle` (install,
  reboot, confirm, boot from B), `update.cut_during_install`,
  `update.rollback_keeps_data` and `shell.write_persists` are tested.

## Basic GUI (Phase 26)

- With `gui` on the cmdline the kernel does not initialize the framebuffer console
  (serial only) and `run_init` hands init an `Mmio` object with the
  GOP framebuffer (`0x10009`, `READ|WRITE`); `system_info(FRAMEBUFFER)`
  returns width, height, stride, format, size and masks. Without `gui` the
  capability exists but the kernel keeps using the screen.
- `aricode-gfx` allocates no memory: `Canvas` wraps `&mut [u32]` with
  a stride and clips everything (`fill_rect`, `blit`, `text`, `cursor`) to
  the canvas and to a clip rectangle (`Canvas::clipped`); `Desktop`
  holds up to 8 windows with z-order, focus, pointer and cascade
  placement, and `compose` paints background, frames (active/inactive title),
  contents (via a caller callback) and cursor; `compose_region` repaints
  only one rectangle over the previous frame, each window drawing only
  inside its frame and the cursor inside `cursor_rect`. `Scene` snapshots
  what a frame depends on besides window contents and `Scene::damage`
  diffs two of them into a `Damage`: at most 8 disjoint rectangles,
  merged when they overlap or overflow, the whole screen past half of it
  (ADR 0046). All tested on the host over `Vec<u32>`, including composing
  two overlapping windows and random event sequences composed both by
  damage and whole, compared pixel for pixel.
- The compositor composes into a DMA back buffer (the whole screen;
  `DMA_MAX_LEN` rises to 8 MiB) and flushes it to the framebuffer, converting
  to RGB if needed, only where something changed (ADR 0046): each frame
  diffs the scene on screen against the current one (pointer, focus,
  windows opened, closed, raised or moved), adds the content of windows
  whose client sent `Present`, recomposes those rectangles and writes only
  them, at most once per 16 ms; changes that arrive sooner join the next
  frame, so an event waits at most one frame. A one-pixel pointer move
  flushes 247 pixels instead of the whole screen. With the test-only key
  `gui_trace` it logs each frame over serial (pixels and rectangles
  flushed) with one pixel sample per window, which the scenarios require;
  without it a frame prints nothing. Events from the `inputd`s arrive
  with the instant each service received them (`Reply::times`), and the
  compositor merges them by that instant before processing them: a mouse
  click precedes the key that came after it even though the devices are
  read in turn.
- `WindowProtocol v1`: the client allocates the content buffer with its
  hardware capability and transfers it in `Create` (the compositor maps it
  once and reads it whenever its content is recomposed); `Present` damages
  the window's content, and only presented pixels are guaranteed on screen;
  `Poll` waits with a deadline for a `Key`, `Click` or `Focus`; `Close` removes it.
  The compositor translates the keyboard (`aricode-input::Keyboard`) and delivers
  only to the focused window: the `gui.two_windows` probe checks that
  the other window sees nothing.
- The xtask QMP probe now sends mouse, click and keys in that order with
  100 ms between phases.

## Application SDK (Phase 27)

- The user runtime (`_start`, static heap, panic handler, `fmt_u64`
  and `fmt_hex`) lives in `aricode-rt`; `user/testprogs` is back to being only
  test binaries and every service depends on the same runtime. The
  eight duplicated `build.rs` files were reduced to `aricode_build::user_program()`,
  which points at the SDK linker script from its own `CARGO_MANIFEST_DIR`.
- `aricode-sys` gains `time` (clock over `SYSTEM`) and `permissions`
  (walk the received handles skipping the gaps left by giving one up,
  check types, close the extra ones).
  `docs/sdk.md` describes a program's full contract.
- The shell receives a `SPAWNER` (init can now transfer its own) and
  runs `run PROGRAM [ARG]`: the child only receives the console, its process,
  its thread and a read-only `SYSTEM`. `hello_app` demonstrates it in
  `shell.commands`.

## Measurements (stage 5)

- `apps/bench` measures from ring 3 the latency of a syscall, of a full
  IPC call to fsd and of a 4 KiB `replace`/`read`, with percentiles
  over the TSC calibrated against `uptime_ms`; `docs/performance.md` fixes the
  method and keeps each version's baseline. `perf.bench` requires that the
  measurements are produced, not their values (they depend on the host).
- Microsecond clock: `apic::init` measures the TSC against the PIT alongside the
  timer and `time::uptime_us` converts it (assumes an invariant TSC shared by
  all CPUs; without calibration it falls back to the 10 ms tick); `sysinfo::UPTIME_US`.
- Recent kernel events (`events.rs`): 64-entry ring with
  processes created, exited and killed (with the reason), IOMMU DMA faults
  and exhausted DMA quotas, timestamped in µs; `sysinfo::EVENTS`
  publishes them to whoever holds `READ` on `SYSTEM` and `diag` shows them
  (`diag.events`). No message or file contents.
- Per-CPU idle: the scheduler accounts the TSC cycles each CPU's idle
  thread is its running thread, at the context switches (incident 0020,
  "Preemptive scheduler" above), and `sysinfo::IDLE` publishes them
  alongside the cycles since the first tick, all sampled at one instant;
  `cpus` shows them as a percentage (not clamped) and `diag` gathers
  `uptime`, `mem`, `cpus`, `ps` and `services --detail` into one report.
- User stack high-water marks (incident 0018): `sysinfo::STACKS`, one
  `StackEntry` per live process (stack size, bytes touched from the top,
  page granular, from the Accessed bits of the stack's page table
  entries); `diag` lists them after the processes, followed by the
  zero-fill memory each process has reserved and backed so far
  (`sysinfo::ON_DEMAND`, ADR 0049).

## Application sandbox (Phase 28)

- `process_spawn` receives in its sixth argument the child's handle count,
  DMA memory quota (`spawn_limits::pack`) and, since ABI 7, the
  `CONSOLE_FIRST` flag (the first handle becomes a session console, ADR
  0047); the process stores the quota
  (`dma_quota`, `dma_used`) and `dma_alloc` charges the size rounded up to a
  page, returning `Limit` when it is exceeded. The quota is cumulative: what was
  reserved is not returned on free (DMA buffers live as long as the
  process). The DMA a process keeps mapped (its own buffers and those of
  clients) is also bounded by the quota: `mmio_map` beyond it returns
  `Limit` (ADR 0038).
- `initrd_read` (syscall 27) copies an initrd file to whoever holds
  `READ` on the `SPAWNER`; init reads `NAME.manifest` and `aricode-manifest`
  parses it (`name`, `caps`, `memory` in K/M, 8 MiB limit).
- The broker is init: `Broker::serve_one` serves `LaunchProtocol v1` in the
  loop that previously only waited for the shell. `launch` denies manifests that
  request system capabilities, asks fsd for a private directory (`Bind`)
  when the application wants `fs`, duplicates the network,
  compositor and input endpoints only if requested, adds `HARDWARE` only when there are
  buffers to reserve and launches with the manifest's quota; it replies with the
  exit code or `Killed` when the program ends, without blocking
  init: `launch` leaves the process in one of the `MAX_RUNNING` (4) slots and
  `poll_launches` replies when it reaps it (ADR 0015). With no free slot the
  launch gets `Limit`. With `Request::DETACH` (`sys::launch::spawn`,
  `spawn` in the shell) the broker replies `Started` when it creates the process and
  nobody waits for its exit. If the program is not in the initrd, the broker
  looks for it installed in AriFS (`apps/NAME.aripkg`, ADR 0017): it copies the
  package through fsd into a DMA staging buffer of `PACKAGE_MAX` (32 KiB,
  what one AriFS replace writes, shared with the installer and the
  updater since incident 0026; a larger file is refused naming both sizes)
  reserved once, verifies it with `aricode-pkg` (Ed25519 signature of the package
  manifest with the development key and SHA-256 of the ELF and of the
  application manifest), applies the policy to the manifest and launches it with
  `process_spawn_from` (syscall 30: `MANAGE` on the `SPAWNER`, `READ`
  on the buffer, ELF offset and length; the kernel copies the
  segments through the physical map and the buffer is free for the next one).
  From the system, `installer` (capability `install`: fsd's main
  namespace, for it alone) installs or updates `apps/NAME.aripkg` from
  `pkgs/` with a single `replace` after verifying and comparing versions (ADR
  0018). If the kernel rejects the `spawn` for any
  reason, the prepared duplicates are closed (the kernel only takes them when it
  creates the process): previously every nonexistent name consumed an entry in
  init's table. The shell receives the broker endpoint instead
  of the `SPAWNER`.
- fsd serves up to 15 *sessions* besides its main endpoint (ADR 0036):
  each `Bind` (only through the main endpoint) creates an endpoint bound
  to a prefix, each `Share` (ADR 0016) one bound to a single file in read
  mode, and each `Attach` one bound to the whole namespace (the `install`
  capability and the shells, instead of a duplicate of the main endpoint).
  A session is one endpoint pair per grant (never shared between launches)
  of which fsd keeps only the receive side, so the kernel reports its client
  gone (ADR 0030) and fsd, probing each session with a zero-timeout receive
  once a second and whenever a table is full, retires it: descriptors and
  replace in progress released, `fs fds reclaimed xN` recorded when it left
  any open (meter `aricode_faults`, shared with netd; `fs fds full`, `fs
  sessions full` and `fs session create` (the kernel could not create the
  session's endpoint pair) record the refusals). `Op::Descriptors` reports the table
  per owner and `diag` prints it under `files` (`descriptors 3/16 open, 16
  reclaimed from dead clients: main 1, root 2`); scenario
  `filesystem.descriptors_reclaimed` (`run fd_leaker`, incident 0011).
- netd serves sessions the same way (ADR 0037, incident 0016): init asks
  `Op::Attach` (16) with the process name as label for the console and SSH
  shells, each sshd generation and every launch with `caps = net`; every
  TCP, UDP, ping and DNS entry belongs to the session that created it
  (`aricode_net::owners::Owned`, host-tested), a request only reaches its
  own sockets (`BadSock` otherwise), and a retired session has its live
  connections reset, its listener freed with its backlog and its waits
  dropped, with `net socks reclaimed xN` recorded. The sweep runs once a
  second, whenever a table is full and before answering `InUse` for a port
  another owner holds. `Op::Sockets` (17) reports the tables per owner and
  `net`/`diag` print `sockets 3 in use: sshd 2, shell 1; 16 reclaimed from
  dead clients`; scenario `network.sockets_reclaimed` (`run sock_leaker`).
  From a private one, `Open`/`Create` qualify the name as
  `prefix/name` (rejecting `/` in the name) and `List` filters and strips
  the prefix. From a shared file, `List` shows only that file,
  `Open` only opens that file, and `Create`, `Write`, `Replace`, `Flush`, `Bind`
  and `Share` reply `Denied`. The broker requests a `Share` for each name
  in the manifest's `read_files` and denies the launch if the file does not
  exist (`sandbox.shared_file`). A shared file is a top-level file, never
  one inside a directory nor a system file (`RESERVED_FILES`), and a
  request whose session is gone is refused (ADR 0050, incident 0028).
  sshd has two sessions of its own: `sshd/` (host key, `random_seed`) and
  a read-only `authorized_keys` that may not exist yet.

- The kernel serial console serializes each message with a spinlock without
  disabling interrupts (`console::write_raw`): previously a kernel `info!`
  and a `console_write` from a freshly created process were interleaved
  character by character. With interrupts disabled the lock is only tried,
  so that a handler never waits for a thread that cannot run.

## WebAssembly runtime (Phase 29)

- `aricode-wasm` parses the binary once (`Module::parse`: types,
  imports, functions, table, memory, globals, exports,
  elements and data) and executes byte by byte with a `u64` value stack and
  a label stack. Block ends are located by skipping immediates on
  entering each block; `br` trims the stack to the label's height and
  keeps the arity; the function's implicit label acts as `return`.
  Limits: 64 nested calls, 16 K values, 50 M steps (`Fuel`). Every
  load, store, `memory.copy` and `memory.fill` is checked against
  the `&mut [u8]` supplied by the host; floating point returns
  `Unsupported`. The host tests use a minimal module assembler
  to cover arithmetic, loops, `if/else` with a result, recursion,
  `br_table`, globals, `call_indirect`, traps and limits.
- `Module::parse` validates the whole module before returning it (ADR 0019:
  typing of each function with an operand stack and control frames, indices
  of types, functions, table, globals, exports and `start`); whatever the
  interpreter does not execute is rejected there. The accepted profile is in
  `docs/wasm-profile.md`.
- `wasm_runner` is a normal broker application: it receives `SYSTEM`, its
  private directory and `HARDWARE`; it reads `demo.wasm` (xtask compiles it with
  `-C target-cpu=mvp -C target-feature=+sign-ext,+bulk-memory` and leaves it in
  `build/`, and `write_test_disk` copies it to `wasm_runner/demo.wasm`), resolves
  imports by name (`aricode.print`, `aricode.uptime_ms`) and
  reserves the linear memory as a DMA buffer within its quota. The module has
  no way to name anything the runner has not imported for it.
- AriFS files grow to 32 KiB capacity to hold modules.

## Cryptography (Phase 31)

- `aricode-crypto` has no dependencies and no `alloc`. Hashes are
  incremental (`update`/`finish`); Ed25519 uses five 51-bit limbs for
  GF(2^255-19), extended coordinates (formulas from RFC 8032 §5.1.4),
  decompression by square root with `(p-5)/8` and scalars modulo L by
  binary long division (slow but simple: a few signatures per
  boot). Verification checked against the three RFC 8032 vectors
  plus tampered signatures and keys and non-canonical `S`.
- `kernel/src/random.rs`: ChaCha20 `Rng` seeded with SHA-256 of eight
  `rdrand` values (if CPUID.1:ECX.30) and 64 TSC samples plus the
  boot tick; each `fill` mixes in another sample (`reseed`). `qemu64` does not
  expose `rdrand`, so in the tests the source is the TSC and the kernel
  says so. `system_info(RANDOM)` serves up to 256 bytes per call.
- Entropy source (ADR 0039): next to the generator, under its lock, the
  kernel keeps a `sysinfo::RandomSource` account: the seed's source
  (`Rdrand` = 1 only if `rdrand` returned plausible data: retried while
  CF = 0, at least two distinct values other than all-0/all-1 bits; else
  `TscInterrupts` = 2), the `rdrand` values and TSC samples mixed in so far and the
  interrupt stirs counted up to the last read. `system_info(RANDOM_SOURCE)`
  (kind 12, 32 bytes) returns it, never any state; `sys::random_source`
  wraps it. netd records `net tcp secret weak` and sshd `ssh entropy weak`
  (before drawing a host key with no carried `random_seed`) when the
  source is not a hardware one, code = the source number; neither refuses
  to run. `diag` prints `entropy  rdrand` or `entropy  TSC + interrupts (N
  samples, M events): weak`. The scenario option `cpu_rdrand` boots
  `-cpu qemu64,+rdrand` (TCG implements it) for the hardware path
  (`crypto.random_rdrand`, `crypto.entropy_rdrand`).
- Carried seed (ADR 0043): AriBoot reads a 32-byte seed from sectors 1
  and 2 of `ARIBOOT` (two copies with a generation, `aricode_ab::seed`),
  writes its successor to the other copy and blanks the old one, and only
  then hands the kernel its share (HMAC of the seed) in
  `BootInfo::carried_seed`. The kernel copies it out at entry, zeroes it
  in `BootInfo` and hashes it into the seed; `RandomSource` (40 bytes now)
  reports the status in byte 32 (`CarriedSeed`), and a carried seed counts
  as strong for netd, sshd and `diag` (`entropy  TSC + interrupts +
  carried seed`). init records `random seed missing`/`unreadable`/`not
  saved` and replaces the stored seed with `RANDOM` output early in the
  boot and after the boot services; images ship none
  (`crypto.carried_seed`).
- xtask gains `keygen`, `sign` and `verify` (seed and public key in hex);
  `keys/dev` holds the development pair that will sign the test images.

## A/B update (Phase 30)

- 48 MiB test disk: `ARI0` (AriFS), `SYSA`, `SYSB`, `UPDATE` (12
  MiB each) and `ARIBOOT` (32 KiB: sector 0 the boot state, sectors 1
  and 2 the carried kernel seed of ADR 0043). The scenario picks the initial state
  with `disk = slots | esp-only | pending-b | b-exhausted | b-corrupt`;
  the ESP failure scenarios use `esp-only`.
- `aricode-ab`: `BootState` (magic, active, pending+attempts, counter,
  truncated SHA-256 CRC), `choose`/`reject`/`confirm`/`schedule` as
  pure functions, and `Image` (`ARISLOT` header + container with a
  signed manifest). All tested on the host, including a foreign signature, a tampered
  kernel and an unsigned container.
- AriBoot (`boot/src/slots.rs`) opens the whole-disk `BlockIo` handles with
  `GetProtocol` (an exclusive open disconnected the ESP's FAT
  driver), parses the GPT with `aricode-gpt`, reads `ARIBOOT`, applies `choose`,
  reads and verifies the slot image with the embedded `keys/dev` public
  key, writes the state (attempt consumed) and hands kernel and initrd
  to `run`; if the pending one fails, `reject` and the active one; if nothing is valid, ESP.
  It adds `boot_slot=X [boot_pending]` to the cmdline.
- init confirms the pending slot at the end of the boot services:
  it locates `ARIBOOT` by reading the GPT through the block service and writes
  `confirm(state, slot)`. `updater` does the reverse with `schedule`.
- xtask strips debug info from kernel and initrd (`strip
  --strip-debug` if available): the initrd goes from 4.8 MiB to 550 KiB and the
  system image fits in the slot.

- Fixes from audit `e9fdf38`: fsd descriptors
  belong to the endpoint that opened them; the broker closes its duplicates with
  a guard on every exit path; init confirms the slot only at the end of
  boot (`init_fail_late` tests the error path); AriBoot does not boot
  a pending slot whose attempt could not be persisted (`unpersisted`); the WASM
  interpreter bounds memory to its current size, zero-initializes it and runs
  `start`; `keygen` creates the seed with mode 0600.

## SMP (Phase 32)

```text
kernel/src/cpu.rs              per-processor Cpu (GDT, TSS + #DF stack, current thread, quantum, syscall scratch), GS base
kernel/src/arch/x86_64/smp.rs  0x8000 trampoline, boot tables, INIT-SIPI, ap_entry, TLB shootdown, halt_others
kernel/src/arch/x86_64/apic.rs ICR: send_ipi, broadcast_ipi, send_init, send_startup; vectors 0x21 RESCHED, 0x22 TLB_FLUSH, 0x23 HALT
libs/sched                     shared queue, per-CPU current/idle, affinity, pending wakeups
```

- Boot: after `task::init` the BSP copies the trampoline to `0x8000` (low
  memory reserved by the PMM and never returned when boot memory is reclaimed),
  builds three table frames (copy of the kernel PML4 + 2 MiB
  identity map) and, for each MADT LAPIC other than its own, creates the idle thread of
  that CPU (its stack is the AP's boot stack), writes the parameters
  (PML4, rsp, `ap_entry`, index) and sends INIT, 10 ms, SIPI, 1 ms, SIPI if
  it is not online yet, and waits up to 500 ms. The AP goes real mode ->
  protected -> long (PAE, `EFER.LME|NXE`), jumps to `ap_entry`, loads the kernel
  CR3 and the BSP's CR0/CR4, installs its GDT/TSS, the shared IDT,
  `syscall`, `IA32_GS_BASE`, its LAPIC (timer with the count calibrated by the
  BSP), adopts its idle thread and enables interrupts. The boot tables are
  freed at the end.
- Per CPU: static `Cpu` with `self_ptr` at `gs:[0]`; `cpu::current_tid()`
  and `cpu::id()` are a single GS-relative access (atomic with respect to
  migrations). `swapgs` in `syscall_entry`/`sysretq`, in
  `interrupt_common` when `cs & 3 == 3` (on entry and on exit) and in
  `usermode::enter` (with `cli` before). `TSS.rsp0` and the `syscall` scratch
  are set on every context switch on the switching CPU.
- Scheduling: only the BSP calls `time::tick` and `wake_due`; the APs only
  consume quantum. When a thread becomes ready (`spawn`, `wake`, sleepers
  waking up) and another CPU is in its idle loop it is sent `RESCHED`; the idle
  loop checks the queue with IF = 0 and sleeps with `sti; hlt` (no window). A
  thread migrates freely unless pinned (`spawn_on`, `set_affinity`).
- TLB: thread stack slots are not reused and a dead process no longer
  has its CR3 on any CPU (every switch to another address space reloads CR3), so
  the normal freeing paths keep using local `invlpg`. For
  unmappings in the kernel half in use by other CPUs there is
  `vmm::flush_page` (local + `TLB_FLUSH` with acknowledgement); the receiver
  invalidates the page or, with address 0, the whole TLB by toggling `CR4.PGE`.
- Panic: `halt_others` sends `HALT` to the other CPUs before the dump, which
  names the CPU; fatal exceptions do too.
- `system_info(CPUS)` and `cpus` in arish. Tests: `test=smp`, `test=idle`
  and `smp.*` scenarios; `cargo xtask test --cpus N` as a stress run of the whole suite.

## IOMMU (Phase 33)

```text
libs/acpi                      DMAR: DRHD (scopes, INCLUDE_PCI_ALL), RMRR
libs/iommu                     registers, Capability/ExtendedCapability, RootEntry, ContextEntry, SecondLevel (3/4 levels), FaultRecord
kernel/src/iommu.rs            units, per-process domains, attach/map_region/unmap_everywhere/release, fault interrupt (vector 0x24)
```

- Boot: after PCI enumeration, `iommu::init` maps each DRHD's registers
  uncached, picks the paging levels (4 if SAGAW allows it, otherwise
  3, the minimum across units), programs the root table (`RTADDR` +
  `SRTP`), the fault interrupt (MSI to the BSP's LAPIC, vector 0x24),
  invalidates contexts and IOTLB and enables translation (`TE`). If the unit is not
  cache-coherent (`ECAP.C = 0`, as in QEMU) every entry
  written is `clflush`ed.
- Domains: one per process, created on the first `dma_alloc`, `mmio_map` of a
  DMA object or device mapping; DIDs from 1 with a free list. The
  second-level tables map IOVA = physical with R+W in 4 KiB pages.
- Assignment: the MMIO objects that `pci::claim` hands out (configuration and
  BARs) carry the function's `source_id`; when mapping one, the kernel writes
  that function's context entry (the bus's context table created
  on demand) pointing at the process domain's tables, maps its
  RMRRs and performs a global invalidation on the unit.
- Release: `DmaBuffer::drop` removes the region from every domain
  before returning the frames; `release_address_space` destroys the process
  domain (context entries set to not present, invalidation, tables back to the
  PMM).
- Faults: the unit writes the FRCDs and fires the MSI; the handler
  decodes them (`bus:dev.fn`, read/write, address, reason), clears
  and counts them; `system_info(IOMMU)` returns units, domains and
  faults.
- QEMU: `-device intel-iommu,aw-bits=48` before the devices and
  `iommu_platform=on` on each virtio device; the transport accepts
  `VIRTIO_F_ACCESS_PLATFORM` whenever it is offered. Lock order:
  `PROCESSES -> IOMMU -> PMM`; never the reverse.
- Tests: `test=iommu` (`dma_rogue`) and `iommu.enabled`; with
  `--cpus 4` the whole suite runs translated under SMP.

## USB HID over xHCI (Phase 25, part two)

```text
libs/xhci                      xHCI/USB structures (no hardware)
libs/input                     HidBootKeyboard / HidBootMouse: boot reports -> evdev events
services/usb-input             usbd: xHCI controller, enumeration, interrupt endpoints (input service) and bulk endpoints (bulk-only storage as a block service, ADR 0023)
```

- init launches `usb_input` for each PCI function of class `0c.03` with the driver
  contract (service, cfg, hardware, MSI-X IRQ, BAR0) plus `SYSTEM`. The driver
  checks CAPLENGTH/HCIVERSION (the capability registers are read as
  32-bit words: QEMU does not handle 16-bit accesses) and, if it is not an
  xHCI, exits with 0 without fuss.
- Memory: one 64 KiB DMA buffer split with a linear allocator
  (DCBAA, scratchpads, 16-TRB command ring, 32-TRB event
  ring, ERST, device contexts and input context, 16-TRB transfer
  rings and reports). With the IOMMU enabled, that buffer
  is all the controller can touch.
- Enumeration per port with a connected device: port reset if it is
  not enabled, `Enable Slot`, `Address Device` with EP0 at the speed's default
  packet size, device `GET_DESCRIPTOR`
  (`Evaluate Context` if the actual packet size differs) and
  configuration `GET_DESCRIPTOR`, `SET_CONFIGURATION`, for each HID boot interface
  `SET_PROTOCOL(0)` and `SET_IDLE(0)`, `Configure Endpoint` with the interrupt IN
  endpoint and four `Normal` TRBs in flight per endpoint.
- Events: the event ring is consumed using the cycle bit;
  `Transfer Event`s are matched to their TRB by physical address, the report
  is decoded (keyboard: modifier and usage differences; mouse:
  buttons, dx/dy, wheel) and the TRB is requeued. `Port Status
  Change` events only clear the change bits.
- A single input device for the whole controller
  (`USB HID Keyboard+Mouse`); `input_client` and the compositor route by
  event type, not by device.
- Selection and publication (audit 4836023): `pci_info_ext` exposes the
  programming interface and init only hands `usb_input` the `0c.03` functions
  with interface `0x30` (the EHCIs, `0x20`, get `usb_ehci`, ADR 0031); any input service is published once it
  answers `Info` (`input_service_ready`), and `InputClient` bounds its
  waits so a dead service does not block the client. The device
  descriptor is first read as 8 bytes to set the EP0 packet size
  with `Evaluate Context`; FS/LS intervals are rounded
  down (3..=10).

## USB 2.0 over EHCI (ADR 0031)

```text
libs/ehci                      EHCI registers, queue heads and qTDs (32/64-bit), legacy handoff, hub class (no hardware)
services/usb-ehci              usb_ehci: EHCI controller, root ports and USB 2.0 hubs, HID (periodic schedule), bulk-only storage (async schedule)
```

- init launches `usb_ehci` for each `0c.03` function with interface `0x20`,
  after every xHCI, with the same contract and extras as usbd. Intel EHCIs
  have no MSI: the driver polls and never uses an IRQ, every 8 ms while
  the boot enumeration runs, HID devices are served or a connection is
  debounced, and otherwise every 250 ms (`aricode_ehci::poll_wait`) or at
  a client read's deadline, whichever comes first: the console bridge's
  20 ms reads still wake it (scenario `usb.ehci_hotplug`: a keyboard
  plugged into a controller with nothing on it is found and enumerated).
- Bring-up per EHCI 4.1 and 5.1 (USBLEGSUP handoff, HCRESET, frame list,
  async head, `CONFIGFLAG`); enumeration of root ports and hubs (the rate
  matching hub of Intel 6/7 series), split transactions through the hub's
  transaction translator for full- and low-speed devices.
- One queue head per endpoint from a fixed pool; qTDs are handed to idle
  queue heads only; async queue heads leave the schedule with the async
  advance doorbell, interrupt ones two frames before reuse.
- Failures per ADR 0028: reported with the qTD token, recorded as
  `ehci ...` events, recovered only as USB 2.0 9.4.5 and BOT 5.3.4 say,
  `strict` dumps and halts.

## Service supervision (stage 1, ADR 0011)

```text
libs/supervisor                states, generations, restart budget with growing backoff, startup deadline
services/init                  Supervisor: Resident {kind, probe, endpoint, process, Service}; poll in the shell loop
sdk/proto::status              per-service status (protocol 7) on the broker endpoint; sdk::status::service_status
```

- init registers each resident service (`virtio_blk`, `fsd`, `virtio_net`,
  `virtio_input`, `usb_input`) with how to relaunch it (`Kind`), its probe
  (`Probe`) and its policy; `launch` creates the process, records the generation and
  waits until it serves; `poll` (on every turn of the shell loop) detects
  deaths with `process_wait(0)`, applies the budget and relaunches when due,
  reclaiming the PCI function again if the previous driver died.
- The service endpoint belongs to init and is handed duplicated to each
  generation: clients do not change handles.
- `restart_storm` (cmdline) restarts `crashy_service` 100 times with a backoff
  of 1 to 8 ms, checking that fsd responds in between and that free
  memory returns to its previous level (`services.restart_storm`);
  `shell.services_detail` covers the queryable status.

## IPC deadlines and correlation (stage 1B, ADR 0012)

- `aricode_sys::rpc::call`: one reply endpoint per request (created and
  closed within the call), send and receive with a deadline, extra handles that are
  consumed or closed if the send fails. All SDK clients use it;
  `launch::run` is the only one that waits for the reply without a deadline (it is the
  program's exit).
- `test=ipc_late` (`ipc.late_reply`): `late_server` answers the first
  request 800 ms late; `late_client` times out at 200 ms, sends another and
  receives its own reply, never the late one, which dies on the already closed endpoint.

## Real hardware (Phase 34, preparation)

- `console_input` also reads the i8042 PS/2 keyboard: initialization
  (ports disabled, configuration with IRQ1 and translation, self-test,
  port 1, `0xF4`), IRQ1 on vector 0x41 and decoding with
  `aricode_input::ScancodeSet1` + `Keyboard` into the same ring as COM1. It is the
  console of a laptop without serial; real keyboards stay in ring 3.
  Without a controller (status 0xFF) it reports and carries on.
- `smp=off` and `iommu=off` on the cmdline skip `smp::start_aps` and
  `iommu::init` with a trace; `test=console_ps2` tests the keyboard with
  QMP input.
- `iommu::init` creates identity domains (keys `u32::MAX - n`) for
  devices with RMRRs and for graphics controllers (RMRR +
  GOP framebuffer) before enabling `TE`, and only then turns on
  translation.
- `cargo xtask usb-image`: `image::write_usb_image` builds the ESP with
  `aricode-fat` (64 MiB, 512-byte clusters, LFN) from the scenario's ESP
  directory, plus ARI0, SYSA (signed image), empty SYSB/UPDATE and
  ARIBOOT; `disk = "usb-image"` boots a scenario from that image without QEMU's
  `fat:` disk. Guide in `docs/hardware.md`.
- First boot on the laptop (2026-10-10): AriBoot opens the GOP
  non-exclusively (`OpenProtocolAttributes::GetProtocol`), like the
  `BlockIO` handles; the exclusive open disconnected the firmware's console
  driver and on real hardware the traces stopped after the slots line.
  For the trace-less window between `ExitBootServices` and the kernel
  console, `FramebufferInfo::beacon` (`Beacon` in `boot-protocol`, which also
  centralizes pixel-format `encode`) paints squares in the top
  row: white before exiting boot services, green before the jump and
  orange in `kernel_entry`, and cyan for each loader step after the
  cmdline; the console clears them when it attaches. Reading table in
  `docs/hardware.md`.
- Second and third photos: the firmware stops at the first `GetMemoryMap`,
  right after allocating with a custom memory type. ADR 0021: AriBoot switches to
  `LOADER_DATA`, BootInfo v4 describes what the kernel keeps and
  `boot/src/memmap.rs` reads the map with traced direct calls.
- Fourth photo (2026-10-10): **first complete boot on hardware**. The
  kernel enumerates 17 PCI functions (ECAM at `0xf8000000`), there is no DMAR, init
  passes its boot tests and ends at `ARICODEOS_KERNEL_READY`. What
  is missing: the xHCI (`8086:1e31`) only has MSI, not MSI-X, and the driver was not
  receiving IRQs (ADR 0022 fixes it); the EHCIs have no driver; without a block
  driver for the USB stick there is no fsd and init does not launch the shell.
- Fifth photo: `claimed (MSI)` and the driver fails at the DMA allocation: the
  controller asks for scratchpads (QEMU for none) and the fixed 64 KiB buffer could not
  fit them. `Params::dma_arena_bytes` sizes it and the driver names the
  failing step.
- Seventh and eighth photos: the USB stick is read over USB, `fs_client` failed
  on flush (the stick rejects `SYNCHRONIZE CACHE`; usbd tolerates it) and
  then **interactive shell on the laptop** with the PS/2 keyboard.
  Backspace printed `?`: the framebuffer console did not understand `\x08` and
  now moves the cursor back one cell.
- Sixth photo: xHCI ready (8 ports, 32 slots, 16 scratchpads), input
  service ok, `ARICODEOS_KERNEL_READY`; no devices on the ports
  because on Intel 7 series the USB ports stay on the EHCIs until
  the system routes them to the xHCI (XUSB2PR/USB3_PSSEN registers). ADR 0023:
  usbd routes the ports, requests the handoff from the firmware and serves the USB stick
  as a block service; without virtio-blk, init mounts fsd on it
  (scenario `usb.storage`).
- Eighth and ninth photos: interactive shell on the USB stick with the PS/2
  keyboard, cursor and history. Network: ADR 0024 moves the stack out to `ari-net-core` and
  adds `rtl-net` for the Realtek `10ec:8136`; without virtio-net, init
  launches it and skips the test clients for QEMU's responder.
- Eleventh photo: **wired networking on the laptop**: `net` shows the
  DHCP address (192.168.1.24), `ping` to the router answers in 1 ms and
  `dns google.com` resolves. The whole stack (ARP, IPv4, ICMP, UDP, DHCP, DNS)
  works on `rtl-net` on the first try.
- Tenth photo: `rtl_net` did not start ("the ELF does not fit in the user
  region"): its `build.rs` was missing and it was linked at the default address.
  `cargo xtask build` now checks the entry point of every initrd
  program, and `diag NAME` + `cargo xtask fs-cat` provide a diagnostic
  channel without photos.

## Boot stack and console (audit 3eb4965)

- The kernel runs on AriBoot's 64 KiB stack until `vmm::init`
  creates its own. Nothing large can live on that stack: the framebuffer
  console (32 KiB grid) is a `static` built with
  `Console::empty()` (`const`) to which the screen is attached with `attach`.
  `check_boot_stack` measures usage just before switching stacks and requires
  16 KiB of headroom; AriBoot and the kernel announce the initrd's SHA-256 to
  detect corruption between the two.
- `wait_until` retires a process only once `release_address_space` has
  marked it `reaped` (address space and DMA domain freed); the IOMMU keeps a
  single owner per PCI function and `release` does not touch contexts that already
  belong to another domain.

## Known debt

- There is no automatic reboot after `updater` nor a test of two chained
  boots in the same QEMU; updates arrive via xtask, not over the network.
- The kernel generator only has `rdrand` and the TSC as sources.
- The WASM interpreter does not validate the module before executing it (it trusts the
  producer; everything is checked at run time) and has no floating point.
- The broker policy is a constant; there are no CPU or handle quotas
  per manifest and the user heap is still static.
- The GUI does not move or resize windows (the model can move them,
  ADR 0046, but nothing asks it to) and has no graphical terminal; the
  shell is still on serial.
- No xHCI/USB HID: only virtio-input devices; the shell still reads
  from the serial console.
- HTTP without TLS, redirects or `keep-alive`; bodies limited to the
  caller's buffer.
- `netd` does not renew the DHCP lease or fragment IPv4; a UDP socket
  holds a single unread datagram; TCP retransmits from `snd_una` without
  SACK or RTT estimation. A client sending to a dead service stays
  blocked on the endpoint queue (there is no close notification).
- `arish` has no history, line editing or redirection; environment
  variables and program launching arrive with the process
  service.
- `fsd` identifies a client by the endpoint its requests arrive on (its
  session, ADR 0036); only init holds the main endpoint (sshd has its own
  sessions since ADR 0050), and its descriptors are released only by
  `Close`. netd
  does the same (ADR 0037): init and the boot test clients share its main
  endpoint, and a session dies with the netd generation that created it.

- The block services keep the last 8 client DMA buffers mapped, by
  physical address, only for their IOMMU entry (ADR 0038); there is no
  IOMMU-only mapping call yet, so each one also costs a CPU mapping.
- The device manager (section 29) lives inside `init`: it enumerates, claims and
  launches drivers; split it into its own service when there is more than one.
- The IOMMU invalidates globally after every change and does not remap
  interrupts; no AMD-Vi variant or domains shared between
  processes.
- `init` does not yet receive endpoints from the services it launches nor can it
  pass them capabilities at creation: this will come with the device manager (Phase 16).
- The benchmarks (section 69) need a reference machine or KVM
  to be absolute; under TCG they only compare versions with each other.
- IPC messages do not carry the sender's identity: a handle to the
  sending process will provide it once there is an identity policy (section 80).
- No SMAP/SMEP: the kernel can touch user memory by accident;
  accesses are limited to validated `copy_from_user`. Enable SMEP/SMAP once the
  reference CPU guarantees it.
- A process has a single thread and its address space is fixed (no `mmap`): the
  `memory_map`/`memory_unmap` syscalls and thread creation from user space
  are part of the set planned in section 22 and will arrive with IPC
  (Phase 14) and the runtime (Phase 27).
- No priorities or load balancing: round robin over a queue
  shared between CPUs, with optional affinity. Device IRQs always go
  to the BSP.
- Stack slots are not reused (one per tid); the reserved virtual
  space allows ~2^31 threads, so it is not a practical limit yet.
- The LAPIC is also covered by the linear physical map with WB caching;
  it is only accessed through the uncached window, but it is an alias to resolve
  when MMIO is reserved outside the linear map.
- No I/O APIC or MSI: no device IRQs yet (Phase 17).
- The heap never returns chunks to the PMM or compacts; the slab/size-class allocator of
  section 16 will come when metrics call for it. Intermediate tables
  created during a failed growth are not returned either (they are reused).
- The loader's tables are recovered in `reclaim_boot_memory` (ADR 0021:
  `page_tables_base/size` in `BootInfo`); the `kernel_ranges()` pages
  are reserved again after freeing boot memory.
- TLB invalidation is still explicit on every unmapping path:
  local (`invlpg`) where no other CPU could have used the translation (heap when
  undoing a growth, thread stacks that are not reused) and with
  shootdown (`vmm::flush_page`) in the VMM self-test and `test=smp`; there is
  no generic mechanism that decides on its own.
- The loader depends on the `uefi` crate (see ADR 0006).

## Power-off and line editing (2026-10-11)

- `aricode_acpi::parse_fadt` and `find_s5` (a scan of the AML for the
  `\_S5_` package, no interpreter) give the kernel the PM1 control blocks and
  the S5 sleep types; `power::acpi_poweroff` enables ACPI mode through
  `SMI_CMD` if needed and writes `SLP_TYP | SLP_EN`. `shutdown` uses it unless
  `qemu_exit` is set (scenario `power.acpi_shutdown`).
- `aricode-lineedit`: the shell's line editor (cursor movement, insert and
  delete anywhere, Home/End, Ctrl-A/E/U, history), redrawing with plain text
  and backspace only; the framebuffer console's backspace wraps to the
  previous row (scenario `shell.line_editing`, host tests).

## SSH (ADR 0025)

- `aricode-crypto`: shared GF(2^255-19) field, X25519 (constant-time
  ladder), Poly1305 and `chacha20-poly1305@openssh.com`.
- `aricode-ssh`: SSH-2 server state machine (curve25519-sha256 with strict
  KEX, ssh-ed25519, chacha20-poly1305, publickey auth, one session channel),
  tested on the host against the OpenSSH client.
- `services/sshd`: port 22 over the network service; host key and
  `authorized_keys` in AriFS. For `shell`/`exec` it asks init (session
  endpoint, `aricode-proto::session`) for an `arish` in session mode, whose
  input and output are IPC endpoints instead of the console. The shell keeps
  unconsumed input between lines (`Input`), so a command and the `exit` that
  follows can arrive in one message.
- Programs the session shell waits for (`run`, the updater, the installer)
  get the session as their console (ADR 0047): the shell sends a duplicate
  of its output endpoint's send side with the launch request (flag
  `CONSOLE`), the broker passes it with `spawn_limits::CONSOLE_FIRST`, and
  the kernel puts a *session console* at the child's `0x0001_0003`. It
  reports `CONSOLE`; `console_write` sends the text to the endpoint in
  messages of at most `IPC_MAX_DATA` bytes (never an empty one, which sshd
  reads as the end of the session), waiting up to 2 s for room per message;
  `ipc_send`, `console_read` and `console_inject` refuse it and it cannot be
  transferred. Detached launches and the console shell keep the kernel
  console. Scenarios: `init.boot` (selftest check with a pair), `ssh.exec`
  (`run console_probe`).
- Tests: `[host_ssh]` in scenarios (generated client key, `authorized_keys`
  installed, a host port chosen per run forwarded to 22, ADR 0040); `ssh.exec`.

## HTTPS (ADR 0026)

- `aricode-crypto`: SHA-384, HMAC, HKDF, ChaCha20-Poly1305 and AES-GCM,
  big integers with RSA PKCS#1 v1.5/PSS verification, ECDSA P-256/P-384.
- `aricode-x509`: DER, certificates, chain verification, host names, 17
  embedded roots. `aricode-tls`: TLS 1.3 client state machine.
- Kernel: CMOS real-time clock read at boot (`rtc.rs`, `sysinfo::WALLCLOCK`).
- `sdk/https` (`aricode-https`): HTTP/HTTPS GET over the TCP service; the
  shell's `http` uses it, with `trusted_ca.der` from AriFS as an optional
  extra root. Scenario `net.https` (`host_https = true` starts the host's
  `openssl s_server`). Since ADR 0048 it follows up to five redirects
  (301/302/303/307/308), each on a new connection within the same budget,
  never from `https://` to `http://`, and holds 16 KiB of response headers
  on the heap (`MAX_HEADERS_LARGE`; github.com sends 10.6 KiB); scenario
  `update.redirected_source`.
- init waits for sshd's `Ready` session message before starting the console
  shell, so their console output never interleaves.

## Network updates (ADR 0027)

- Channel: `build/update/{manifest.txt, aricodeos-N.img}` from `cargo xtask
  publish-update`, served by `serve-update` or any web server.
- `apps/updater` (then caps system, fs, net, block): channel manifest, version
  comparison with the active slot, streaming download into `UPDATE`
  (`aricode-https::get_with` over `aricode-http::fetch_with`, with the
  run's remaining time as the fetch budget, ADR 0033), then the
  Phase 30 verification and install. Shell `update [URL|check|apply]`.
- Kernel `map_foreign` is idempotent per region; servers map client buffers
  per request (`block::map_client_buffer`).
- Scenario `update.network` (`host_update = true`, disk `update-empty`).

## Update sources and signed index (ADR 0029)

- `libs/update-index` (`aricode-update-index`, no_std + alloc, host tests):
  index format and strict parser, Ed25519 verification before parsing,
  rollback serials, version comparison and the downgrade rule, best offer
  across sources, the source list (normalization, migration from
  `update_url`), stored index copies, the installed/plan/staged lists the
  updater and the installer exchange, the history, and the argument
  constants of both programs.
- `apps/updater` (caps system, fs, net, block; no main file space):
  `REFRESH` fetches and verifies each source's `index`/`index.sig`
  (bounded, with a run deadline), stores verified copies in
  `updater/indexes`, reports and saves the plan in `updater/state`; `UPGRADE` re-verifies the
  copies, installs only planned items: the system image (memory download,
  size, SHA-256, signed manifest and version checked, slot header zeroed
  first and written last) and packages staged as `updater/pkg-N.aripkg`
  with the `[staged]` section of `updater/state`. `APPLY` installs the `UPDATE` partition under the
  downgrade rule. Unknown installed system versions count as downgrades.
- `apps/installer`: `LIST` writes `[installed]` of `updater/state`; `STAGED` installs
  only the staged list (checked against it, then emptied);
  `ALLOW_DOWNGRADE` only with `STAGED`; history lines and fault events.
- `aricode_crypto::trust`: the build's public key (development by default,
  `--signing-key DIR` through `ARICODE_SIGNING_PUB`) for AriBoot, init, the
  installer and the updater.
- Shell: `update`, `update add|remove|sources|apply [--allow-downgrade]`,
  `upgrade [--allow-downgrade]` (installer `LIST` before the updater,
  installer `STAGED` after it).
- xtask: `publish-update [--version N | --no-system] [--app DIR:VERSION]
  [--no-legacy-manifest] [--signing-key DIR]` (atomic, legacy
  `manifest.txt` for the transition release), `usb-image --update-source
  URL --signing-key DIR`, scenario table `[host_update]`.
- Scenarios: `update.sources`, `update.migrate_legacy_url`,
  `update.network`, `update.tampered_index`, `update.tampered_image`,
  `update.stale_index`, `update.refuse_downgrade`, `apps.upgrade_network`,
  `apps.downgrade_refused`.

## Development update channel (ADR 0045)

- Two channels, two keys: the release key signs `stable`, the development
  channel's key (`keys/dev-channel/`, seed outside the repository) signs
  builds of `develop` or a branch. `aricode_ab::Image::verify_with` and
  `aricode_update_index::verify_channel` bind the key to the channel the
  manifest or the index names.
- `aricode_crypto::trust::dev_channel_key` (`ARICODE_DEV_CHANNEL_PUB`):
  in every updater; in AriBoot only with `usb-image --dev-source` (and in
  test builds); never in init or the installer. AriBoot writes
  `BootState::loader_dev_channel` on every boot; the updater uses the
  development key only with that flag (`trusted_keys`), and fetches
  development indexes only from `dev URL` lines in `updater/sources`.
- The development manifest and index add `channel = dev` and
  `commit = HEX`; the version is the build number, assigned by
  `cargo xtask publish-update --channel dev` (counter beside
  `build/update-dev/`). AriBoot passes `boot_version`, `boot_channel` and
  `boot_commit`; `diag` prints the running system.
- Shell: `update [--channel stable|dev]`, `update add [--channel dev] URL`,
  `upgrade [--channel stable|dev] [--allow-downgrade]` (only `--channel`
  moves the system to the other channel), `update rollback` (updater
  `ROLLBACK`: schedules the inactive slot's verified image, no network).
- The stable floor (`[floor]` of `updater/state`): the newest stable
  release the machine ran; a stable release below it is a downgrade,
  channel switch or not. A development build number seen with another
  commit is refused. xtask numbers development builds per host
  (`~/.cache/aricodeos/dev-build`, locked).
- Scenarios: `update.dev_channel`, `update.dev_wrong_key`,
  `update.dev_rollback`, `update.dev_stable_image`, `update.stable_floor`.

## DHCP address probe (2026-10-12)

After `DHCPACK` the network service probes the address with an ARP request
(sender 0.0.0.0) for 600 ms before using it (RFC 2131 4.4.1). An answer from
another MAC means the address is already in use on the LAN (a stale lease
or a static host): the service sends `DHCPDECLINE`, logs the offending MAC
and asks again ten seconds later. Found on the laptop, whose lease
collided with a phone using a randomised MAC.
