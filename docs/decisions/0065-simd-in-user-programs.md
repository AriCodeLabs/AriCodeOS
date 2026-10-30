# ADR 0065: SIMD in user programs

- **Date:** 2026-10-30
- **Status:** Accepted. Step 2 of ADR 0063's order of work. The owner chose
  on 2026-10-30 to build user programs with a stable target rather than
  move them to nightly.

## Context

User programs were built for `x86_64-unknown-none`, like the kernel. That
target is soft-float: the compiler emits no SSE, and the AES-NI, SHA and
vector intrinsics fail to compile ("LLVM ERROR: Do not know how to split
the result of this operator"). So no implementation, ours or an adopted
one, could use the instructions that make cryptography 8 to 76 times
faster on the same machine (ADR 0063, the host measurements of
2026-10-29).

The kernel did not support them either. `switch_context` saved only the
integer registers the System V ABI requires. If a program had used SSE,
the next process would have seen its registers, and could have changed
them.

What established systems got wrong here:

- **Lazy switching.** The registers are left in place on a switch and
  `CR0.TS` set; the first SIMD instruction of the next thread faults
  (`#NM`) and only then is the state swapped. Speculative execution read
  the previous owner's registers past the fault (CVE-2018-3665, LazyFP).
  Linux, the BSDs and Windows moved to eager switching.
- **AMD's x87 error pointers.** `FXSAVE` and `XSAVE` on AMD processors
  without `XSAVEERPTR` save the last x87 instruction and data pointers only
  when an exception is pending, and `FXRSTOR` does not reset them. The next
  process could read where the previous one had been (CVE-2006-1056).
  Linux resets them with a harmless x87 load before each restore
  (`X86_BUG_FXSAVE_LEAK`).
- **Leaking a fresh process's registers.** A new process must start with
  clean registers, not the last owner's.

## Decision

### Kernel: eager state

1. **Enabled on every CPU.** `CR0.EM` and `CR0.TS` are cleared, `CR0.MP`
   and `CR0.NE` set, and `CR4.OSFXSR` and `CR4.OSXMMEXCPT` set. With XSAVE
   the kernel also sets `CR4.OSXSAVE` and enables x87, SSE and AVX in
   `XCR0`, as far as the CPU has them. AVX-512 and AMX stay disabled:
   their state is 2.5 to 11 KiB per thread, and nothing needs them yet.
2. **One area per user thread** (`arch::x86_64::fpu::FpuArea`), 64-byte
   aligned and sized from CPUID: 512 bytes with `FXSAVE`, 832 with
   `XSAVE` for x87, SSE and AVX. A new area holds the state after reset:
   x87 control word `0x037F`, MXCSR `0x1F80`, every register zero.
3. **Saved on every switch, never lazily.** The scheduler saves the
   leaving user thread's registers (`FXSAVE`, or `XSAVEOPT` when the CPU
   has it) and loads the next user thread's (`FXRSTOR` or `XRSTOR`),
   with the scheduler lock held and interrupts off. `CR0.TS` is never set
   and `#NM` never happens.
4. **Kernel threads have no area.** The kernel is built soft-float and
   never touches these registers, so they hold the last user thread's
   state while it runs. Unlike Linux, the kernel needs no
   `kernel_fpu_begin`.
5. **A redundant load is skipped.** When a CPU still holds the next
   thread's registers (it ran there last and only kernel threads ran
   since), the load is skipped, as Linux does. The save is never skipped,
   since the thread may run elsewhere next. Each area remembers the CPU it
   was last loaded on; each CPU, the thread whose registers it holds.
6. **AMD error pointers are reset before each restore** on AMD and Hygon
   processors without `XSAVEERPTR`, with Linux's sequence (`fnclex`,
   `emms`, `fild`). A program then reads fixed kernel addresses there
   instead of another process's, the trade-off Linux makes.
7. **SIMD and x87 exceptions in ring 3** (`#XM`, `#MF`) end only that
   process, like any other ring-3 exception.
8. **ABI-aligned entry.** `_start` is entered with `rsp + 8` a multiple of
   16, as if called. SSE code relies on that alignment for its stack
   slots. Before this the stack was entered aligned to 16, which only
   soft-float code tolerated.

### User programs: a stable target with SSE

9. **User programs are built for `x86_64-unknown-linux-gnu`, freestanding**
   (`USER_TARGET`, `USER_RUSTFLAGS` in `tools/xtask/src/build.rs`):
   - `relocation-model=static` and `crt-static`: a static ELF at
     `0x400000`, as before;
   - linked by the toolchain's own `rust-lld`. No host C toolchain or C
     library takes part, so a libc symbol fails the link;
   - the flags come from xtask, not `.cargo/config.toml`, because the
     host's own builds share the triple. Cargo gives them to target crates
     only, never to build scripts.

   Nothing of Linux is used: the target only selects the code generation
   (SSE2 baseline, red zone, System V ABI).
10. **What libc provided on that target, the runtime provides**
    (`aricode-rt`):
    - `memcpy`, `memmove`, `memset`, `memcmp` and `bcmp`, adapted from
      compiler-builtins (`sdk/runtime/src/mem.rs`, MIT OR Apache-2.0).
      They are the routines `x86_64-unknown-none` programs ran before, so
      copies cost the same (64 KiB in 873 ns natively, both builds);
    - `_Unwind_Resume` and `rust_eh_personality`, which the precompiled
      `alloc` names because it was built to unwind. Programs abort on
      panic, so neither is ever reached; if one were, the process exits as
      on a panic.
11. **Guarded.**
    - `cargo xtask check` refuses crates that bind to Linux or libc
      (`libc`, `getrandom`, `rustix`, `linux-raw-sys`, `nix`, `errno`) in
      any user program's dependencies, following `cfg(unix)` ones too
      (`tools/xtask/src/deps.rs`).
    - The list includes raw system call crates (`syscalls`, `sc`,
      `origin`, `linux-syscall`), and out-of-tree apps are checked too
      (`check_external`, at `app-pack`).
    - Every built program, external apps included, must be a static
      executable (`check_user_elf`) with:
      - no `PT_INTERP`, `PT_DYNAMIC` or `PT_TLS`;
      - no loaded relocation section, such as the `IRELATIVE` ones a
        statically linked C library brings.
12. **ADR 0008 holds.** Everything builds with stable Rust. A custom target
    with SSE would need nightly (`-Z build-std`). Turning `soft-float` off
    on `x86_64-unknown-none` is a deprecated toggle that changes the ABI
    under the precompiled `core`, and rustc warns it will become an error.

## Measurements

QEMU TCG on the reference host (AMD Ryzen 7 5800X), `cargo xtask perf`,
median p50 of three runs, 1 CPU. **Kernel only** is the eager kernel with
user programs still soft-float. Times in µs:

| Figure | Before | Kernel only | After |
|---|---|---|---|
| syscall `thread_get_tid` | 0.428 | 0.319 | 0.339 |
| IPC fsd list | 77 | 72 | 72 |
| read 4 KiB | 205 | 200 | 197 |
| IPC net info | 84 | 79 | 80 |
| SHA-256, 16 KiB | 172 | 172 | 159 |
| ChaCha20-Poly1305, 16 KiB | 225 | 224 | 195 |
| AES-128-GCM, 16 KiB | 2589 | 2550 | 2397 |
| Ed25519 verify | 1209 | 1211 | 1280 |
| X25519 | 357 | 359 | 368 |
| memcpy, 64 KiB | 15 | 15 | 15 |

Four CPUs give the same picture. The eager save has no measurable cost in
IPC: the differences are within TCG's run-to-run noise, and none is a
slowdown. TCG emulates SIMD instructions in helpers, so it understates
what SSE code gains on hardware.

The same `aricode-crypto` code, built both ways and run natively on the
host through raw Linux system calls, best of five:

| Figure | `x86_64-unknown-none` | `x86_64-unknown-linux-gnu` |
|---|---|---|
| SHA-256, 16 KiB | 302 MB/s | 349 MB/s (+15 %) |
| ChaCha20-Poly1305, 16 KiB | 258 MB/s | 279 MB/s (+8 %) |
| AES-128-GCM, 16 KiB | 25 MB/s | 25 MB/s |
| Ed25519 verify | 192 µs | 214 µs (11 % slower) |
| X25519 | 58.5 µs | 62.5 µs (7 % slower) |
| memcpy, 64 KiB | 873 ns | 873 ns |

Two results are slower: Ed25519 and X25519. LLVM's vectorizers are not the
cause; turning them off changes nothing. Our field elements (`Fe`, five
`u64` limbs) are passed by value, and with SSE the compiler copies them
with 16-byte moves right after writing the limbs with integer stores (382
SSE moves in the build, none before). That is the usual store-forwarding
stall.

This code is what step 3 of ADR 0063 compares with the dalek crates, which
verify in 25 µs against our 204 µs on the same host. It is kept as it is
until that comparison.

The stripped user binaries together grow by 0.2 % (2,371,456 to 2,376,280
bytes).

## Consequences

- Programs can use SSE and AVX, with runtime CPU detection for anything
  past the SSE2 baseline: AES-NI, PCLMULQDQ, SHA, AVX2. An implementation
  adopted under ADR 0063 runs its accelerated path.
- Every user program is built for a new target. Out-of-tree apps are built
  through `cargo xtask app-pack`, which sets the flags. A plain
  `cargo build --target x86_64-unknown-linux-gnu` of a user program fails
  at the link instead of producing a wrong binary.
- `rust-toolchain.toml` and the Docker image install
  `x86_64-unknown-linux-gnu`.
- The boot log names the mechanism, for example
  `cpu: user FPU state saved eagerly with xsave, xcr0 0x7 (832 bytes per thread)`.

## Limits

- A floating-point operation that `core` lowers to a C library call
  (`fmod` for `%` on floats, `floor` without SSE4.1) no longer links, since
  that target leaves those calls to libc. No program uses one today. One
  that does fails at the link, and the routines would come from an adopted
  `libm` (ADR 0063).
- The red zone (128 bytes below `rsp` in leaf functions) is new to user
  programs. Ring-3 interrupts switch to the kernel stack, so nothing
  clobbers it; the stack check (`tools/xtask/src/stack.rs`) does not count
  it.
- The AMD error-pointer reset is not observable under TCG, which does not
  have the quirk.

- QEMU's TCG records an unmasked SIMD exception in MXCSR but does not raise
  `#XM` (checked with QEMU 10.0: MXCSR read back as `0x1d84`). The
  scenarios accept that one case and say so in the log. On hardware the
  kernel takes the generic ring-3 exception path, which `#MF` exercises
  under TCG.
- The skipped load (decision 5) trusts that nothing writes a thread's
  area while the thread could still have its registers loaded on a CPU.
  Nothing does today. A debugger that edits registers would have to drop
  the CPU's ownership first.
- AVX-512 and AMX stay disabled until a measured need justifies their
  state size.

## Tests

- `fpu.isolation` (1 CPU, `qemu64`: `FXSAVE` and the AMD error-pointer
  reset):
  - three `fpu_probe` processes check that they start with clean registers
    and an ABI-aligned stack;
  - each loads its own pattern into the x87 control word and data
    registers, MXCSR and every XMM register;
  - each yields or sleeps 300 times and checks it gets the pattern back;
  - an unmasked x87 division by zero ends only its process (`#MF`).
- Every round's pattern is different (no two instance and round pairs
  share register contents). Every 50th round spins without system calls
  until the timer preempts it, so involuntary switches are checked too.
- `fpu.isolation_smp` (4 CPUs):
  - six probes, migrating between CPUs when they sleep;
  - then two probes that sleep in every round leave CPUs idle. A probe
    woken elsewhere comes back to a CPU that ran only its idle thread and
    still holds the probe's older registers: the case where a skipped
    load would be wrong.
- `fpu.xsave` (4 CPUs, `cpu_simd`): the same on the `XSAVE` path with AVX;
  the probes also check the YMM upper halves.
- Mutations that fail the scenarios:
  - disabling the save and restore in the scheduler fails all three ("x87
    registers left by another process", "x87 control word changed");
  - dropping the `last_cpu` check of the skipped load fails
    `fpu.isolation_smp` in the sleeper phase.
- Host tests:
  - a user program reaching `libc` through a `cfg(unix)` dependency is
    refused, but not through a build or dev dependency;
  - an ELF with `PT_INTERP` or `PT_DYNAMIC`, a position-independent one,
    or one with truncated program headers is refused;
  - `PT_TLS`, or a loaded `SHT_RELA` or `SHT_REL` section, is refused; an
    unloaded one is not.

## Audit (2026-10-30)

An independent audit found no high findings. It found no path where one
process sees another's registers or a thread runs on state not its own:
creation, first run, exit, migration, AP start, the skipped load, area
reuse and `XSAVEOPT`'s optimisations included. Its findings and what was
done:

- **M1.** The SMP scenario could not catch a wrong skipped load. Every
  round loaded the same pattern, so stale registers matched, and CPUs were
  never idle while a probe ran elsewhere.
  - Fixed: a different pattern each round, a preempted spin round, and the
    sleeper phase.
  - Dropping the `last_cpu` check passed before the fix and fails after.
- **L2.** The x87 error pointers are not checked, and the reset leaves
  kernel addresses there. Documented in decision 6 and the limits.
- **L3.** Hygon processors lacked the error-pointer reset. They have it
  now.
- **L4.** A statically linked C library would have passed the ELF check.
  Raw system call crates were not refused, and out-of-tree apps were not
  checked. All three are covered now (decision 11).
- **L5.** The stack check does not count the red zone. Documented.
- **L6.** Floating-point libm calls no longer link. Documented in the
  limits.
- **L7.** `XSAVEOPT`'s optimisations and `#XM` delivery run only on
  hardware: TCG treats `XSAVEOPT` as `XSAVE`.
