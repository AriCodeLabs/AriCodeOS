# ADR 0080: One path from programs to the hardware

- **Date:** 2026-11-06
- **Status:** Accepted as the direction on 2026-11-06, with the owner's choices
  below. The research behind it wrote no kernel or driver code, built nothing and
  ran nothing on the laptop.

## The goal, and the short answer

The owner: every operating system works piecemeal, each layer sets its own
protocols, and so they become slow. They want one standard path for graphics,
performance measurement, 3D simulation with integrated robotics control, and
tensors, updating old standards and drivers. A game demonstrates it. Names and
brands do not matter.

**The thesis is half right.**

- **Right:** fragmentation is expensive, and on Linux the seams between
  subsystems have blocked improvements. The fence and memory contract has held
  up user-mode queues for years. A new system can avoid that seam.
- **Not right:** the number of layers is not where the time goes. A translation
  layer can stay close to native. The large measured gains came from removing
  work inside a thick driver. The largest user-visible cost today is shader
  compilation, and no single interface removes it, because each vendor's
  instruction set is different.
- **So:** define one small model (memory, queues, fences, compute and render in
  the same queue), adopt SPIR-V, keep encoding and compilers per vendor, and
  measure against Linux on the same laptop. If the figures do not match or beat
  Linux, the claim is not made.

Labels: **[M]** measured or counted by this study; **[D]** documented by a
primary source that was read; **[S]** seen only through a search summary;
**[I]** inferred here.

## The owner's choices (2026-11-06)

- **One interface as the base, tunable above it, with no hardcoding:** "hardcoding is
  always where everyone fights". Limits, queue sizes, timeouts and policies are
  parameters with defaults that a program or the owner can change and read back,
  never constants a user must patch. What hardware fixes (register layouts, a
  manual's restrictions) stays in tables, by family.
- **Debian on the laptop for reference runs: yes.** It is a test machine, and anything
  may be installed on it. Nothing from it reaches production, and no data on it may
  endanger other machines: see `docs/hardware.md`, "The reference laptop is a test
  machine".
- **Graphics tests as needed, always with a thermal watch,** and always compare: if
  another system does better, find out why and improve ours, for more speed at no
  more resource use.
- **The robot is open:** a drone, an arm, a printer or a toy. So the control interface
  must not assume one. The first target is a simulated arm; a real device follows.
- **Second GPU target:** tests run on what is at hand; a machine bought for it is still
  to be decided.
- The rest of the table below stands as recommended, open to revision.

## Decisions for the owner

| # | Question | Recommendation |
|---|---|---|
| 1 | **Order.** Start now, or after the desktop (ADR 0078)? | Finish 0078 stage 0, then run this next to 0078's stage 1. Composition stays in software. Stage A here does not touch the desktop |
| 2 | **The one interface.** Our own small protocol, or Vulkan itself, or WebGPU? | Our own small `gpu` protocol and Rust library, with semantics that are a subset of Vulkan, and SPIR-V for shaders. No new shader format. A full Vulkan layer stays possible later |
| 3 | **Pay Khronos?** Conformance costs USD 95,000 to 120,000 and is the only route to the name and the patent grant | Not now. Do not use the name or claim conformance. Ask a lawyer before any commercial release, because without adoption there is no Khronos patent grant |
| 4 | **The laptop has no IOMMU.** Whoever writes GPU page tables can reach all of memory. May a signed user-space driver hold that? | No. The kernel writes GTT and PPGTT entries, checked against the memory capabilities the driver presents. The driver keeps everything else |
| 5 | **Isolation between programs on the HD 4000.** Linux enabled one GPU address space per process on this family at least twice and went back | One GPU client at a time on this family, with a state clear at each change of client. Real multi-client isolation starts at the second target |
| 6 | **Is the HD 4000 a compute target?** | No: a first real test, and the proof of the path. Stop at a compute compiler subset and enough 3D for the game. No full Vulkan on it |
| 7 | **Second target.** | Intel Gen9 to Gen11 integrated graphics: public manuals, full page tables, no firmware needed to submit work. It needs a machine: may we buy a used one? NVIDIA Turing or later through GSP is third, for real tensor speed |
| 8 | **Real-time in the kernel.** Priorities, one-shot timers and time donation across IPC change the scheduler | Yes, in that order, after a measurement on the laptop shows the baseline. It gets the kernel audit |
| 9 | **ROS 2.** Inside the system, or at the edge? And which real robot is the goal? | At the edge: a bridge speaking its protocol. Inside, a small `control` protocol shaped like ros2_control. Please name the robot, because rate and bus decide much of stage G |
| 10 | **A reference run on the laptop.** May a Debian live stick boot on it, to run Mesa on the same hardware? | Yes. Without it, "faster than Linux" can never be checked (ADR 0063). It is your action: it needs the stick moved |

One correction to this study's brief: **user programs do have floating point.** ADR 0065
gave them SSE2, and the kernel saves x87, SSE and AVX state per thread. What is missing is
`libm`, and any way to create a second thread: the ABI has no such call [M: `sdk/abi`].

## Context

What this system has today, read from `develop`:

- no engine driver: BAR 0 is mapped in the kernel only, for one step at boot (ADR
  0079); every pixel comes from the CPU;
- one thread per process; round-robin, one ready queue, no priorities, a 100 Hz tick
  (`libs/sched`, `kernel/src/time.rs`);
- DMA buffers: contiguous, at most 8 MiB, under a quota, transferable (ADR 0038);
- `pci_claim` clears bus mastering (ADR 0038). That could stop a pipe scanning out, so
  the normal claim cannot be used on this GPU (device study, step 4);
- static ELFs only: `check_user_elf` refuses `PT_DYNAMIC` (ADR 0065), so a loader that
  opens a vendor library at run time does not exist;
- signed driver packages are designed (ADR 0073). Trust labels a driver and never
  grants capabilities;
- the reference laptop has no DMAR table: no IOMMU protects it from DMA.

## 1. What makes existing stacks slow or fragmented

| Claim | Evidence | Verdict |
|---|---|---|
| "API layers are the cost" | DX11 to DX12 in 3DMark's API overhead test: Titan X 2.7 M to 14.6 M draws/s; R9 290X on a weak CPU 0.73 M to 7.44 M [S 9]. But that compares thick and thin *drivers*, not layer counts. Futuremark warns against comparing vendors with it; one commenter's rule of thumb is that 25 % more draws may be 5 % more frames [S 9]. Zink (OpenGL over Vulkan, one layer *more*) was in "an increasingly tight race" with the native RadeonSI by 2022, and faster in some operations [S 10]. DXVK figures I found are anecdotal, and they swing with vendor drivers both ways [S 11] | **Mostly false as stated.** Work done per call matters: state validation, implicit sync, residency. Layer count alone does not |
| "A syscall or context switch per submit" | No published per-submit figure for Mesa on Intel was found. Microsoft says user-mode submission is for "applications that submit small workloads frequently", and keeps presentation on kernel submission [D 5]. Our own arithmetic [I]: at the 79 µs p95 of `ipc_fsd_list` in `tests/perf/baseline-1cpu.txt` (QEMU TCG, not hardware, and it includes a small file service request), one submit per frame is 0.5 % of 16.7 ms; 10,000 small dispatches per second, each submitted alone, would be 79 % | **True only for small, frequent work.** Irrelevant to a frame; decisive for tensors and control loops unless work is batched |
| "The kernel does work per submit" | i915's submit call takes a buffer list [I: general knowledge]. ANV moved relocations to user space because the kernel forced a full CPU sync between batches [S 12], and groups command buffers to cut calls [S 12]. VM_BIND designs take binding out of the submit path so its cost stops depending on buffer count, stated as a design claim [S 12] | **True, and the real lesson.** Bind memory once, submit cheaply |
| "Shader compilers" | Valve: extensions avoid "many causes of frame hitches due to PSOs being late-created" [D 13]. Local caches over 100 GB and vendor stores in terabytes, by one expert's account [D 14]. No measured before and after was found | **True, and the largest user-visible cost.** It comes from per-state recompilation and many permutations. One API does not cure it; fewer baked states do |
| "Memory management" | Linux: "Mixing indefinite fences with normal in-kernel DMA fences does not work", because memory management can create deadlocks user space cannot see [D 6]. Attempts to fix this have run since at least 2022 [S 7]. A 2024 Panthor thread notes that pinning all memory makes the problem easy [S 7] | **True, and a real seam.** Eviction is what makes fences hard. A system that pins GPU memory avoids it, at the cost of no overcommit |
| "Copies" | No measurement found. Zero copy is possible in principle on HD 4000: LLC is shared (`has_llc`, Linux [M 3]) and our DMA buffers already move between processes | **Unmeasured.** Stage B measures it |
| "Too many APIs" | CUDA was at most about 30 % faster than OpenCL on 16 benchmarks, and the authors blame unfair comparison [S 15]; others found CUDA consistently ahead, or compilers responsible [S 15]. That points to compiler maturity. tinygrad runs AMD and NVIDIA compute on one small queue layer: `ops_amd.py` 65 KiB, `ops_nv.py` 54 KiB [M 16] | **True for engineering effort, weak for run time.** A small shared compute path is possible. It is not free of vendor code |
| "API complexity" | Aaltonen (2025): modern GPUs are bindless and coherent, so an API could be 64-bit pointers plus indices into texture heaps. His PSO initialization goes from about 400 lines to 18 [D 14]. One expert's argument, not a measurement | **Plausible, and useful as a design guide** for what to leave out |

- **Linux DRM + Mesa:** best public record of what went wrong. NIR already shares compiler
  work between vendors. The seam is fences against memory.
- **WDDM/DirectX:** a default 2 s timeout, then a reset without a reboot [D 5]: the strongest
  precedent for surviving GPU faults. User-mode submission is still in development in WDDM
  3.2, and needs hardware doorbells [D 5].
- **Metal:** one API for render and compute, on hardware one company controls. No primary
  figures read. It shows the model works, not that the model is why it is fast.
- **Level Zero:** "lowest-level" compute, and it says most programs should not need it [D 8].
- **CUDA:** one vendor. Its strength is compiler and libraries, which an OS does not replace.
- **WebGPU:** "strictly validating all the commands before they reach GPU", usually with a
  GPU process [D 17]: it pays validation and IPC to run hostile code.

### The microkernel precedents

| | Shape | Lesson |
|---|---|---|
| **Fuchsia Magma** | Client library compiles shaders and formats command buffers; a privileged system driver programs the hardware; buffers are ordinary kernel memory objects; semaphores are kernel events; the connection is the isolation boundary; on error the connection dies and Vulkan reports device lost. Requires unified memory [D 18]. An Intel driver is in the tree [M 18] | Closest model to ours. Licence to be checked per file before adopting anything |
| **Genode** | An experimental Gen8 multiplexer in about **4,200 lines**, written in four months, with Mesa's i965 at about 78,000 lines on the client side [D 19]. Each client has its context, page tables and part of the aperture. By 21.08 the display driver is a separate, disposable component above the multiplexer, because one chip has both functions [D 19] | Thin privileged part, thick client. Separating display from engines is a known need. Read for ideas only: Genode's licence is AGPL |
| **tinygrad** | A user-space driver stack for AMD and NVIDIA compute, MIT [M 16]. Reports of a stack without vendor scheduling firmware are unverified [S 16] | A very small compute-only path is possible |

## 2. What to adopt, what to define

### Licences

| Standard | Terms | Fit |
|---|---|---|
| **SPIR-V** | Headers MIT-style [M 20]. Trademark Khronos. Patent terms not verified | **Adopt** as the shader contract |
| **Vulkan** | Spec may be copied unmodified, "no licenses to any patent, trademark" [S 21]. Headers Apache-2.0 OR MIT [M 20]. Patent licence only after passing conformance. Adoption USD 95,000 members, 120,000 others; renewal 60,000 and 75,000 [D 22]. I found no open-source exception on that page | **Adopt the semantics**, not the name |
| **WebGPU** | W3C Software and Document License, W3C Patent Policy [D 17]. `wgpu` and `naga`: GitHub reports Apache-2.0 [M 20]; a dual MIT licence is to be checked | **Offer later** as a library on top. `naga` has `no_std` since v26 [S 23]; `wgpu-core` unverified |
| **Level Zero** | Loader MIT [M 20]; the spec page states no licence [D 8] | **Copy the shape** of immediate command lists; compute only |
| **CUDA, Metal, DirectX** | Vendor-owned, one vendor or one OS | No. Windows code never (ADR 0063) |
| **Mesa** | Intel code read here is MIT [M 3] | **Read, compare, adopt in parts.** C/C++: we do not build it (memory `no-building-downloaded-c-code`) |

Intel's manuals may not be used for derivative works, as in ADR 0079: our code and
documents describe them in our own words. `genxml/gen70.xml` in Mesa is an MIT
machine-readable command list for this family [M 3], a useful check against the manual.

### The one interface, at which level

**Not** a new universal command stream. **A small model, one level above the hardware:**

```text
program      libs/gpu: Rust API, one for all vendors         no syscall per command
             SPIR-V for shaders (naga reads it)
per vendor   client backend: encodes commands into memory    in the program
             compiler: SPIR-V to the vendor's instructions   in the program, untrusted
protocol     gpu v1 over IPC: info, vm, map, queue, submit   one per submission
per vendor   gpu driver process: rings, reset, power         kind = driver (ADR 0073)
kernel       memory objects, page-table writes, IRQ, MMIO windows, stop on death
```

- **Memory:** one kind of object, pinned, not necessarily contiguous, charged to a quota,
  transferable by capability. Never overcommitted in v1, so no eviction and no fence
  against memory problem.
- **Addresses:** buffers have GPU virtual addresses and shaders use pointers, as modern
  hardware allows [D 14]. HD 4000 can do a limited form: 32-bit offsets through its
  stateless model, binding table index 255 (Vol 4 Part 1, 3.2.2). Textures are indices.
- **Queues:** one model; a queue's kind is render-and-compute or copy. On HD 4000, render
  and GPGPU share one ring, with `PIPELINE_SELECT` and a flush between them (Vol 1 Part 2,
  5.3.8). **So tensors and graphics really can share one queue, on the hardware we have.**
- **Fences:** a timeline: a monotonic value the GPU writes to memory the client may read,
  plus a wake-up from the driver. The kernel and driver never trust a value a client
  could write.
- **Work:** batches built in client memory, many dispatches in one batch. A submit is one
  IPC; on hardware with doorbells, none.

**Stays per vendor:** instruction sets and compilers, command encoding, page-table formats,
memory types, caches, reset, firmware, power. Claiming otherwise is what makes "one
standard" fail.

**Open point, decided by measurement at the second target:** how a program gets its vendor
backend without dynamic loading: (a) link every backend in, simple and large; (b) a signed
client blob the loader maps at a fixed address; (c) a neutral command stream the driver
translates, as a browser's GPU process does, giving vendor-free binaries but making the
driver parse hostile input. With one backend, (a) needs no decision yet.

A **CPU backend** of the same interface runs on QEMU, which does not emulate this GPU (ADR
0079), as IREE uses SwiftShader [S 24]. It gives host and QEMU tests and an honest baseline.

## 3. How a capability microkernel fits

| Need | Design | Authority |
|---|---|---|
| GPU memory | New kernel object `Mem`, pinned, scatter-capable. Handle rights as today | Kernel |
| GPU address spaces | PPGTT tables in kernel-owned frames, never mapped writable to anyone. The driver asks `gpu_map(vm, va, mem, rights)`; the kernel checks the handle and writes entries | Kernel |
| Global GTT | Lives in the upper 2 MiB of BAR 0 (Vol 3 Part 2, 1.10). Same rule: never mapped to the driver. Display entries are untouchable | Kernel |
| Registers | Windows of BAR 0 for ring and GT pages only, not display pages, not the GTT half. `DE_IER` and `GT_IER` share page `0x44000` (Vol 3 Part 3), so that page stays in the kernel or is brokered | Kernel grants, driver uses |
| Claim | A new form that leaves the command register alone while a pipe runs (ADR 0079 state) | Init |
| Commands | Client builds batches in its own memory, without a syscall per command | Client |
| Submission | IPC to the driver, which writes the ring. On Gen7 the ring is privileged, so clients never touch it | Driver |
| Fault of a client | Driver reads fault registers, resets the engine, kills that client's session: "device lost" | Driver |
| Death of the driver | Kernel stops the engines before freeing frames (ADR 0038's rule) with a small per-family stop routine, and leaves bus mastering on for scanout. ADR 0038's stop of bus mastering is not safe with display on the same function: the split Genode reached in 21.08. Supervisor restarts the driver; clients see device lost | Kernel, supervisor |

**On a machine without an IOMMU, page-table writes are the whole security question.** With
an IOMMU, a driver bug stays in its own domain. On the laptop, one bad PTE reads or writes
anything. Keeping the writes in a few checked kernel lines means a driver bug can hang the
GPU, not corrupt the kernel. ADR 0073 already refuses DMA to unofficial drivers on such
machines unless the owner confirms.

### Risks by design

| Design | Risk | Mitigation or verdict |
|---|---|---|
| Driver in the kernel (Linux, WDDM's kernel part) | Every GPU bug is a kernel bug | Rejected (ADR 0005) |
| Driver process writes page tables | On the laptop it holds all of memory | Rejected for this machine (decision 4) |
| Clients submit raw batches, hardware checks them | Non-secure batches cannot reach global GTT, and a physical-address store raises a command error (Vol 1 Part 2, 5.3.7.6). But hardware checks are incomplete on later families: **Linux runs a software parser on the Gen9 blitter** (`i915_cmd_parser.c` [M 3]; CVE-2019-0155, number from memory [25]) | Fine on Gen7 for render. On Gen9 do not expose the copy engine directly, or parse |
| Software command parser | A parser of hostile data in a privileged process. On Gen7 Linux's parser exists mainly to *allow* register writes that hardware would drop [M 3] | Skip on Gen7. Clients lose those registers; the driver sets what they need |
| Residual state between clients | **CVE-2019-14615:** EU registers and shared local memory leak across contexts on Gen7 to Gen9 [S 25]. Linux clears them with a GPU kernel on context switch (`gen7_renderclear.c`, 454 lines [M 3]). The first fix hurt performance badly, later reduced [S 25] | One client at a time: clear only when the client changes. We need our own clear kernel |
| One VM per process on Gen7 | **Linux enabled this, re-enabled it in 2019 after preallocating page tables [S 4], then in February 2020 reverted: "Full-ppgtt on gen7 is proving to be highly unstable and not robust"** [M 4]. Current `i915_pci.c` still has `INTEL_PPGTT_ALIASING` for Ivy Bridge [M 3]. The manual says directory entries may change only for non-active contexts (Vol 1 Part 2, 2.2.1), and they sit inside the GGTT | Decision 5: switch address spaces only with the engine idle. Treat as a risk, not a solved problem |
| Hung or hostile shaders | Gen7 preempts only at thread-group boundaries, and needs cooperating commands in the batch (Vol 2 Part 2, 1.3.5). An endless kernel cannot be preempted | A timeout and a domain reset (`GDRST`: render, media, blitter; Vol 1 Part 6, 1.4.8). Windows' 2 s is the reference. A program pays with device lost |
| User-mode queues (later hardware) | Client rings doorbell without the driver seeing it. Fences are client-writable. Windows monitors progress on the timeout cadence [D 5] | Only on hardware with real doorbells and scheduling. Never decide memory freeing from a client fence |
| Compiler in the driver | Hostile SPIR-V in a privileged process | Compile in the client. The compiled kernel is untrusted anyway: the hardware is the boundary |
| Timers | GPU timestamps enable timing attacks; WebGPU coarsens them [D 17] | Recorded risk. A capability for high-resolution figures, closed by default (memory `security-no-open-doors`) |

## 4. Hardware reality

### HD Graphics 4000, from the manuals

Identity: `8086:0166`, Ivy Bridge mobile GT2 (`i915_pciids.h`). BAR 0 f7800000 (4 MiB:
2 MiB registers, 2 MiB GTT), BAR 2 e0000000 (256 MiB), 64 MiB stolen (read from the laptop,
ADR 0079). Sources: Intel PRM Ivy Bridge, 2012, by volume below.

| Resource | What it offers | Source |
|---|---|---|
| Execution units | 16 EUs, 8 threads each (GT1: 6 and 6) | Vol 1 Part 1, 3.2.6 |
| EU data | 32-bit float; double (first time an 8-byte type exists, IEEE mode only); half only by conversion | Vol 4 Part 3, 1.3 |
| Render | Fixed pipeline with VS, HS, TE, DS, GS, stream out, clip, SF, WM. Mipmaps up to 8192 × 8192 | Vol 2 Part 1; Vol 4 Part 1 |
| Compute | `GPGPU_WALKER`, `GPGPU_OBJECT`, hardware barriers, indirect dispatch (with a zero-size workaround); shared local memory 64 KiB per half-slice | Vol 2 Part 2 |
| Memory | Global GTT plus two-level PPGTT, up to 2 GiB; 40-bit physical; 32 KiB pages; tables not snooped | Vol 1 Part 2, 2.2, 4.6 |
| Engines | Render, blitter, video, each with a ring. Reset per domain | Vol 1 Parts 3 to 6 |
| Time, faults | 80 ns timestamp; page fault status bit and data registers | Vol 1 Part 3 |

**Worth it for compute?** Not for speed.

- Peak single-precision rate [I]: 16 EUs × about 8 operations per clock × about 1.1 GHz is
  roughly 140 GFLOP/s, double with multiply-add. The PRM gives neither FPU width nor clock.
- A dual-core AVX CPU is roughly 80 GFLOP/s [I]; the laptop's model is not in our records. Both
  share one memory bus, so ratios of 2 to 3.5 on paper shrink on memory-bound tensors.
- Mesa warns "Ivy Bridge Vulkan support is incomplete" today [M 3], and it was never conformant
  [S 26]. Even a veteran stack does not offer full Vulkan here.

**Worth it as the first test?** Yes. Public manuals, the machine we own and test on, all the
design questions appear on it, and one ring serves render and compute.

### What a first engine driver needs

Basis for size: Linux's closest files are 6,248 lines together (`intel_ring_submission.c`
1,459, `intel_reset.c` 1,728, `intel_ggtt.c` 1,682, `gen6_ppgtt.c` 469, `gen6_engine_cs.c`
456, `gen7_renderclear.c` 454 [M 3]), much of it for other families. Genode's whole
multiplexer was about 4,200 lines [D 19].

Device study (skill `aricodeos-device-study`): this ADR covers steps 1 to 3. Steps 4 to 8 fall
to each stage's ADR, including the resource table: of the above, nothing is used today. Check
first: forcewake and RC6 before GT registers, and Linux's quirks in `intel_ring_submission.c`.
**No register is guessed:** the read-only probe comes first.

### Other vendors

| Family | Documents | Firmware | Fit |
|---|---|---|---|
| **Intel Gen9, Gen11** (Skylake to Ice Lake) | PRMs public: SKL, KBL, ICL, RKL, DG1 at x.org [M 27]. Fuchsia's Intel driver to read, family coverage not checked [M 18] | None needed to submit: Linux defaults GuC off before Gen12 [M 3] | **Best second target.** Full 48-bit page tables (`INTEL_PPGTT_FULL`) [M 3], common used laptops. Same vendor's compiler lineage (`brw`, 82,648 lines [M 3]) |
| **Intel Tiger and Rocket Lake** | RKL yes. TGL not listed at x.org [M 27] | GuC off by default in Linux [M 3] | Good later |
| **Intel Alder Lake-P on, Arc** | ACM at x.org [M 27] | GuC submission default [M 3] | Doorbells; firmware needed |
| **AMD RDNA** | ISA guides public; they cover shaders only, and AMD treats its Linux driver and register headers as the documentation [S 28]. Linux has at least **522 MiB of register headers** [M 3] | Firmware expected; terms not verified | Real speed, poor documents. tinygrad is a useful comparison [M 16] |
| **NVIDIA Turing or later** | `open-gpu-doc` MIT, manuals up to Ampere [M 29]. Open kernel modules, Turing or later, with GSP firmware [M 29]. NAK in Mesa, 54,077 lines including Rust [M 3]. `nova-core` is in Linux: GPL, read-only | GSP firmware does much of the work; redistribution terms not verified | **Best third target.** Most tensor value; the host has one (`10de:2d04`, GB206 [M: `lspci`]). Blackwell runs Vulkan 1.4 under NVK [S 30]. Testing means booting the development host, or a second card |

### Effort, in person-months of an experienced GPU engineer

Ranges with stated bases, not promises. There is no figure yet for this project's own pace on GPU work.

| Work | Range | Size in Rust [I] | Basis |
|---|---|---|---|
| A: first blit. Probe, forcewake, ring, GTT calls, fence, timeout, reset | 2 to 4 | 3 to 5 k lines | Genode: 4 months, 4,200 lines, with contexts and PPGTT |
| B: compute, hand-assembled. Render ring, PPGTT, state clear, EU assembler | 2 to 4 | 3 to 6 k | Same base; PRM Vol 2 Part 2 is short |
| C: compute compiler subset, `gpu v1`, `libs/gpu`, CPU backend | 4 to 9 | 15 to 30 k | NAK 54 k and `elk` 91 k lines are full stacks |
| D: 3D through to a shaded scene | 6 to 12 | 15 to 30 k | `vulkan_hasvk` 41,502 + `isl` 21,046 + `blorp` 13,434 lines [M 3]; we need a fraction |
| E: the game | 2 to 4 | | [I] |
| F: threads, memory object, perf | 2 to 4 | | Kernel work with audits, as ADR 0072 and 0074 [I] |
| G: real-time and control | 5 to 10 | | [I]; small in lines, long in audits |
| Second target, Gen9 | 8 to 18 | | Rings to execlists and page tables, plus a compiler backend. NVK reached Vulkan 1.0 in November 2023 with a paid team [S 30]; its start date was not checked |
| NVIDIA with GSP | 12 to 24+ | | [I] Honeykrisp reached Vulkan 1.3 in **one month**, but on a reused compiler and kernel driver [S 31]: reuse dominates this figure |

Reference laptop path to the demo: **roughly 23 to 47 person-months.** A smaller demo is
possible by dropping the shader compiler for graphics and hand-writing the game's shaders.

## 5. Tensors

What a tensor program needs from this path: large dense buffers (beyond 8 MiB, hence
`Mem`); compute dispatch with many small jobs in one batch; a compiler; and the number types the
hardware has. HD 4000 has 32-bit float and double, half only by conversion, and 64 KiB of
shared local memory per half-slice. Its fit for quantized 8-bit models was not studied.

| Candidate | Licence [M 20] | Language | On this path? |
|---|---|---|---|
| **ONNX** (format) | Apache-2.0 | Spec | **Adopt** as the model format |
| **burn** + **CubeCL** | MIT OR Apache-2.0 | Rust | **Best fit.** CPU backend `burn-flex` is `no_std` [M 32]. CubeCL emits for CUDA, ROCm, Metal, Vulkan, WebGPU [M 32]. A new CubeCL target for `gpu v1` is the natural seam |
| **tract** | MIT/Apache per README | Rust | **Reference for CPU.** Pure Rust ONNX and NNEF, embedded targets, some GPU backends [M 32]. `no_std` not verified |
| **candle** | MIT/Apache per README | Rust | CPU, CUDA, Metal, WASM [M 32]. No Vulkan path seen. Comparison only |
| **ggml** | MIT | C/C++ | Has Vulkan, WebGPU and CPU backends [M 32], and no run-time allocations by its README. Good source to read and a reference to compare against; not built here |
| **IREE / MLIR** | Apache-2.0 (with LLVM exception, recheck) | C++ | Compiles to SPIR-V for a Vulkan HAL; needs timeline semaphores [S 24]. Too large to host here. Possible offline: compile on the host, run SPIR-V here |
| **tinygrad** | MIT | Python | Comparison, and proof of a small stack |

Recommendation: stage F runs `burn-flex` or `tract` on the CPU: useful alone, no GPU needed, and
the figure every GPU claim is compared against. Both are measured before adoption (ADR 0063),
including size and behaviour on one thread. Float functions need a `libm` crate.

## 6. Robotics simulation

### Standards

| Standard | Terms | Use |
|---|---|---|
| **URDF** | XML; parser BSD [M 20] | **Adopt** to import robots: the widest use |
| **MJCF** | MuJoCo's format, Apache-2.0 project [M 20] | **Adopt** for comparison with MuJoCo |
| **SDFormat** | Apache-2.0 [M 20] | Later, for whole worlds |
| **ROS 2** | Apache-2.0 [M 20]. Wire choices: DDS (Fast DDS Apache-2.0; Cyclone's licence to check) or Zenoh (EPL-2.0 or Apache-2.0 [M 20]). `rmw_zenoh` is fully supported since Kilted, and appears as default in the newer Lyrical docs [S 33] | **At the edge, not inside** (decision 9). ROS 2's client libraries sit on C (`ros2_rust` wraps them, not verified in detail) |
| **ros2_control** | Apache-2.0 [M 20]. A manager runs read, update, write; hardware components expose state and command interfaces; one writer per command interface [D 34, S 34] | **Copy the shape,** not the code |

### The physics engines

| Engine | Licence [M 20] | Language | Notes |
|---|---|---|---|
| **Rapier** | Apache-2.0 | Rust | Declares `no_std` [S 35]. Its repository has `rapier3d-urdf`, `rapier3d-mjcf`, `mjcf-rs` [M 35]. Multibody joints. **Robotics accuracy unmeasured:** no study was found |
| **MuJoCo** | Apache-2.0 | C/C++ | The reference for robotics. Best in robotics tests in a paper by its own authors [S 36], best in accuracy and linear stability in a 2023 study, with PhysX and DART ahead on other tests [S 36] |
| **MuJoCo Warp, Newton** | Apache-2.0 | Python, on Warp | GPU, "designed for NVIDIA hardware" [S 36]. Not on this path |
| **PhysX 5, Jolt, Bullet, Drake** | BSD-3, MIT; the last two check per file | C++ | References only; PhysX's GPU path is CUDA |
| **Genesis** | Apache-2.0 | Python | Its 43 M FPS drops 150× to 0.29 M in one critic's corrected run [S 36]. **A lesson for us:** state the scene, solver and substeps with every figure |

Plan: adopt Rapier inside. Measure it on the host against MuJoCo from `pip`, which the rule on
downloaded C code allows, on a Franka model: step time, energy drift, joint tracking error. If
Rapier's error is too large for control work, the owner decides between a MuJoCo-style solver
in Rust and running the simulator on a host.

### What the scheduler must add

Today a thread woken while another runs may wait for the 10 ms tick [I: from the code].
Franka's interface runs at 1 kHz [S 37]. In this order, each measured:

1. **A latency measure first:** a `cyclictest`-style program on the laptop under load, and
   the same tool under PREEMPT_RT (merged in Linux 6.12 [S 37]) on the Debian stick.
2. **Threads**, needed by everything here.
3. **One-shot timers** (LAPIC or TSC deadline) instead of the fixed tick for sleepers.
4. **Fixed priority classes** with preemption on wake.
5. **Time donation across IPC.** seL4 MCS [D 38]: a passive server runs on its client's
   scheduling context, so a control thread does not wait behind a low-priority request.
   Ideas only: seL4's kernel is GPL.
6. **Locked memory:** first-touch zero pages (ADR 0049) would fault inside a control cycle.
7. **CPU reservation:** a CPU with no ordinary threads.

Target for the laptop [I]: 1 kHz, p99.99 wake latency under 100 µs, no deadline miss in an
hour. Firmware SMIs may make that impossible on this machine; if so, we report it.

### The same controller on simulator and on real hardware

```text
controller ---- control v1 (shared Mem + tick) ----+--- sim process (Rapier)
  holds a joints capability, nothing else          +--- hardware driver (bus)
```

- **`control v1`:** a shared block of state and command interfaces, as in ros2_control;
  each cycle is read, update, write, with one writer per command.
- **Time comes only from the tick.** The controller has no clock capability, so simulated
  and wall time look the same to it, and simulated runs repeat exactly.
- **The launcher decides who serves the capability.** The controller's binary does not change.
- **What differs is stated:** the sim server injects measured latency and noise, recorded with
  each figure. Perfect timing would flatter the controller.
- **Safety stays out of the controller.** The hardware server has a watchdog: a missed tick is
  a safe stop. An actuator capability never goes to a program with network access unless the
  owner says so. A real robot needs a stop independent of all this software: this ADR does
  not make the system safety-certified.

## 7. One way of measuring

Build on `cargo xtask perf`, `apps/bench` and the baseline files. The rule of
`docs/performance.md` stands: QEMU figures compare builds only. One line format, a superset of
today's `name unit p50 p95 p99`, with required tails, since a control loop or a frame is judged
by its worst moments:

```text
# machine=<id> build=<commit> clock=<source> cpus=<n> scene=<name>
gpu.submit_to_start ns <p50> <p95> <p99> p999=<v> max=<v> n=<samples>
```

| Area | Figures | Clock |
|---|---|---|
| `cpu.`, `ipc.`, `disk.` | Today's, with a map from the old names | TSC |
| `ui.` | ADR 0078's: glyph, line, frame | TSC |
| `display.` | Flip, full and damage; underruns; vblank misses | TSC, pipe bits |
| `gpu.` | Submit to start, start to end, fence to wake, empty batch, copy MB/s, draw and dispatch rates, reset and restart time | GPU 80 ns timestamp, correlated to the TSC |
| `tensor.` | Matrix multiply, ONNX reference models, per backend | TSC, GPU |
| `ctl.`, `sim.` | Wake latency, cycle time, deadline misses, jitter; step time and real-time factor, with scene and solver named | TSC |
| `power.` | Only if measured on hardware | RAPL unverified |

1. **Every GPU or tensor figure has a CPU figure beside it, and a Linux figure where possible**
   (decision 10), same scene, same machine.
2. **A timeline as well as totals:** one frame or control cycle in Chrome trace JSON, which
   Perfetto (Apache-2.0) reads off the machine, to answer "where did the time go?".
3. **`machine=` selects the baseline.** QEMU keeps its wide tolerances; the laptop gets its own,
   run over SSH with no other load.

This also tests the thesis. The `gpu.` submit figure, against Linux's on the same chip, says
whether a shorter path matters. It may say no.

## 8. Stages

Each stage ends with something shown. A to E and H are in order of dependency; F and G are side
tracks. **Stage A works on the reference laptop.**

| Stage | Work | Shown at the end | Needs |
|---|---|---|---|
| **A. A job on the real engine** | Read-only probe. `gpu_intel` as a driver: blitter ring, kernel-written GTT, a minimal `Mem`, fence, timeout, reset of the blitter domain | `run gpu_probe` fills and copies with the blitter, CPU and GPU figures side by side. A hung batch resets. Killing the driver leaves the desktop up | Decision 4; a new claim form; device study; audit |
| **B. Compute, no compiler** | Render ring, one-client PPGTT, residual clear, small EU assembler in `libs/` | Matrix multiply from a hand-assembled kernel, `tensor.` against the CPU, copies measured. **The first real answer on HD 4000** | A |
| **C. SPIR-V compute** | `libs/gpu`, `gpu v1`, `naga` front end, Gen7 back end for a compute subset (compared with Mesa's `elk` by source), shader cache in signed packages | One compute shader runs unchanged on the CPU backend under QEMU and on the laptop's GPU | B |
| **D. 3D** | Pipeline state; a hand-written triangle; a shaded mesh through SPIR-V; presented without a copy if measurement agrees | A spinning model at a stated frame rate, beside a Mesa run of the same scene | B; C for compiled shaders |
| **E. The game** | Small Rust 3D game: drive a simulated arm or rover, physics from G, rendering from D, an optional tensor policy from C, figures live on screen | One program using graphics, measurement, simulation, control and tensors on one path, on a 2012 laptop. It also runs on the CPU backend, showing what the GPU buys | D, F, G's simulator |
| **F. The base** (side) | Threads, `libm`, `Mem` proper, the `perf` format, CPU tensors | An ONNX model on the CPU, in QEMU and on the laptop; new figures in `perf` | Nothing here. Threads get the audit |
| **G. Control** (side) | Latency measure; scheduler steps; `control v1`; Rapier and URDF; ROS 2 bridge off the machine | A simulated arm tracking a path at 1 kHz with a jitter figure; the same binary against a fake hardware server; then a real device if the owner names one | Threads from F |
| **H. Second target** | Gen9: execlists, full PPGTT, multi-client isolation, second backend, backend delivery chosen | The same game on two GPU generations. Only here is the interface really tested | D; a machine |

```text
A ──> B ──> C ──> D ──> E <── G (sim, control) <── F (threads)
                  └───────> H
F (perf format, CPU tensors) ──> every comparison above
```

### What can really proceed in parallel

- **Truly parallel, on the host:** the `perf` format, CPU tensors, Rapier against MuJoCo, the EU
  assembler, the compiler front end, `control v1`'s host tests. They touch no kernel code.
- **Not parallel:** kernel changes (threads, `Mem`, scheduler, GTT calls) follow one another, each
  with audits; one QEMU suite at a time; one laptop, whose driver and latency runs cannot overlap.
  More agents do not change this: the laptop is the bottleneck.
- **The desktop (ADR 0078)** overlaps only in `Mem` and the compositor. Software composition stays
  the fallback for good: a machine with no GPU driver loses nothing.

## Options rejected

- **A new universal GPU language or command stream first.** No users, compiler or test suite.
  SPIR-V has all three.
- **Full Vulkan now.** About 200,000 lines of C, C++ and XML for this family in Mesa (`hasvk`, `isl`,
  `blorp`, `elk`, `genxml`) [M 3], still "incomplete" there. The interface stays open to it.
- **Port Mesa whole.** C and C++, no build here, not `no_std`. Parts may come to Rust after comparison.
- **WebGPU as the only interface.** Its validation suits hostile web code. Keep it as a library on top.
- **Start from a current NVIDIA or AMD card.** Most speed, but no test machine apart from the
  development host, firmware dependence, weaker documents. The first target must be one we can break.

## Consequences

- **New kernel surface:** `Mem`, page-table calls, threads, timers, priorities, a claim form. Each is
  more trusted code and gets its audit (skill `aricodeos-test-levels`).
- **Trust:** `gpu_intel` is a `kind = driver` package, `serves = gpu`, limited by the kernel and not
  by its signature (ADR 0073).
- **Tests:** only the CPU backend runs under QEMU; the rest needs the laptop, as each stage must say.
- **A 2012 chip pulls designs backward.** The interface targets pointers and Gen9 onward, with HD 4000
  mapped onto it, not the reverse.
- **Risk of never finishing:** the demo is two or more person-years for one expert. A, B and F are each
  worth having if work stops there, which is why they come first.

## What this study could not verify

- **No hardware run:** every HD 4000 statement is from manuals or Linux. Clock, FPU width and the
  laptop's CPU model are unknown here.
- **No per-submit cost** was measured or found published for Mesa on any Intel chip. The figure most
  central to the thesis does not exist yet; stage A and the Debian run give it.
- **[S] items are search summaries,** not pages read. Phoronix and the mail archive refused the fetch
  tool; the 2020 revert was downloaded directly [M 4]. Check them before quoting elsewhere.
- **Licences marked "check":** Cyclone DDS, Bullet, Drake, IREE's exception, GitHub's NOASSERTION
  entries, AMD and NVIDIA firmware redistribution, SPIR-V patent terms. Verify per file before adopting.
- **A Khronos fee waiver for open-source projects:** the page shows none; Mesa is conformant, and I did
  not find how.
- **Metal:** no primary source was read. **Effort figures** come from a few cases and carry large error.

## Sources

Fetched 2026-11-06. Downloads are in `~/aricode-scratch/gpu-study/`, treated as untrusted data.

1. Intel, *Programmer's Reference Manual, Ivy Bridge*, 2012, rev. 1.0: Vol 1 Parts 1 to 7, Vol 2 Parts 1 and 2, Vol 3 Parts 1 to 4, Vol 4 Parts 1 to 3. <https://www.x.org/docs/intel/IVB/>
2. This repository, `develop` at dbc42d0: ADR 0005, 0038, 0049, 0063, 0065, 0073, 0078, 0079; `sdk/abi`, `libs/sched`, `kernel/src/pci.rs`, `tests/perf/`, `docs/performance.md`.
3. Linux (MIT parts) and Mesa, main, read 2026-11-06. `drivers/gpu/drm/i915/`: `i915_pci.c`, `i915_cmd_parser.c`, `gt/gen7_renderclear.c`, `gt/gen6_ppgtt.c`, `gt/intel_ring_submission.c`, `gt/intel_reset.c`, `gt/intel_ggtt.c`, `gt/uc/intel_uc.c`. Sizes from GitHub's tree API, whose listing was truncated, so Linux sizes are lower bounds. Mesa `src/intel`, `src/nouveau`: lines counted from GitLab archives; `vulkan_hasvk/anv_device.c` holds the warning.
4. C. Wilson, "drm/i915/gtt: Downgrade gen7 (ivb, byt, hsw) back to aliasing-ppgtt", 2020-02. <https://lists.freedesktop.org/archives/intel-gfx/2020-February/230829.html>. Earlier steps: <https://lab.nexedi.com/kirr/linux/-/commit/8c50f10d73b50139dcfe48bc22f2c8c7822c1983>, <https://git.evolution-x.org/anierin/msm-5.15/commit/3cd6e8860ecd>
5. Microsoft, *User-mode work submission*. <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/user-mode-work-submission>; *TDR*. <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/timeout-detection-and-recovery>
6. Linux, *Buffer Sharing and Synchronization*, "Indefinite DMA Fences". <https://docs.kernel.org/driver-api/dma-buf.html>
7. König, Vetter and others on dri-devel, 2022: <https://lore.kernel.org/dri-devel/Yo48Z%2FRvnm30up41@phenom.ffwll.local/T/>. Panthor thread, 2024: <https://lkml.rescloud.iu.edu/2411.1/03009.html>
8. oneAPI Level Zero, introduction. <https://oneapi-src.github.io/level-zero-spec/level-zero/latest/core/INTRO.html>
9. 3DMark API overhead results, 2015. <https://www.pcr-online.biz/directx-12-vs-dx11-vs-mantle-early-tests-show-big-performance-gains/>, <https://aws.futuremark.com/news/compare-directx-12-mantle-and-directx-11-with-3dmark>, <https://gamersnexus.net/guides/1885-dx12-v-mantle-v-dx11-benchmark>
10. Phoronix, Zink against RadeonSI, 2022. <https://www.phoronix.com/review/zink-radeon-august2022>
11. Forum and blog DXVK figures. <https://redlib.hbubli.cc/r/allbenchmarks/comments/p1079h/testing_dxvk_vulkan_wrapper_with_acodyssey_using>
12. Mesa commits: <https://lists.freedesktop.org/archives/mesa-commit/2016-November/068286.html>, <https://gitlab.neroreflex.duckdns.org/NeroReflex/mesa/src/commit/0a7224f3ff7542f1560f06759dbdc375e298e853>. Nouveau VM_BIND: <https://lists.freedesktop.org/archives/nouveau/attachments/20230725/4ec4e96b/attachment.htm>
13. Khronos, *Reducing Draw Time Hitching with VK_EXT_graphics_pipeline_library*. <https://www.khronos.org/blog/reducing-draw-time-hitching-with-vk-ext-graphics-pipeline-library>
14. S. Aaltonen, *No Graphics API*, 2025-12-16. <https://www.sebastianaaltonen.com/blog/no-graphics-api>
15. Fang, Varbanescu, Sips, ICPP 2011, and related work it and others cite: <https://arxiv.org/pdf/1005.2581> (Karimi), <https://www.global-sci.com/jics/article/view/15278> (Memarzia, Khunjush), <https://hgpu.org/?p=5748>
16. tinygrad, MIT; sizes from GitHub's tree. <https://github.com/tinygrad/tinygrad>. Summary of claims: <https://aiwiki.ai/wiki/tinygrad>, <https://www.phoronix.com/news/Tiny-Sovereign-Stack-AMD-Close>
17. W3C, *WebGPU*, Candidate Recommendation Draft, 2026-09-15. <https://www.w3.org/TR/webgpu/>
18. Fuchsia, Magma design. <https://fuchsia.dev/fuchsia-src/development/graphics/magma/concepts/design>; driver at <https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/graphics/drivers/msd-intel-gen/>
19. Genode release notes. <https://genode.org/documentation/release-notes/17.08>, <https://genode.org/documentation/release-notes/21.08>
20. Licences from the GitHub API and each `LICENSE` on 2026-11-06: `KhronosGroup/SPIRV-Headers`, `Vulkan-Headers`, `gfx-rs/wgpu`, `oneapi-src/level-zero`, `onnx/onnx`, `iree-org/iree`, `ggml-org/ggml`, `huggingface/candle`, `tracel-ai/burn` (`Cargo.toml`), `sonos/tract`, `dimforge/rapier` (`Cargo.toml`), `google-deepmind/mujoco`, `NVIDIA-Omniverse/PhysX`, `jrouwe/JoltPhysics`, `ros/urdfdom`, `gazebosim/sdformat`, `ros2/rclcpp`, `ros-controls/ros2_control`, `eclipse-zenoh/zenoh`, `eProsima/Fast-DDS`, `google/perfetto`.
21. Vulkan specification, preamble. <https://registry.khronos.org/vulkan/specs/1.3/html/chap1.html>
22. Khronos, *Adopters*, fees and IP Framework. <https://www.khronos.org/conformance/adopters/>
23. wgpu v26 release notes. <https://newreleases.io/project/github/gfx-rs/wgpu/release/v26.0.0>
24. IREE, *HAL: Vulkan and SPIR-V* (an older mirror). <https://opensecura.googlesource.com/3p/openxla/iree/+/25dc6c19ea6e8f83089c90a7fa2911d0a6ac329f/docs/vulkan_and_spirv.md>
25. CVE-2019-14615 and Gen7 clearing: <https://www.phoronix.com/news/Intel-iGPU-Leak-Details>, <https://lore.kernel.org/all/871rrg9dv4.fsf@intel.com/T/>, <https://www.phoronix.com/review/intel-gen7-hit>, <https://www.phoronix.com/news/Intel-Recovers-Gen7-Loss>. CVE-2019-0155 from general knowledge of the Gen9 blitter requirement; the code is in [3], the CVE number not re-read this session.
26. Mesa, "anv: don't advertise vk conformance": <https://lists.freedesktop.org/archives/mesa-commit/2021-October/125112.html>; <https://www.phoronix.com/news/Intel-HASVK-Old-Vulkan-Gen7-8>
27. Index of Intel PRMs. <https://www.x.org/docs/intel/>
28. AMD ISA docs: <https://gpuopen.com/amd-isa-documentation>; comment on documentation: <https://www.phoronix.com/news/AMD-RDNA3-ISA-Guide>
29. NVIDIA: <https://github.com/NVIDIA/open-gpu-doc>, <https://github.com/NVIDIA/open-gpu-kernel-modules>
30. NVK: <https://www.collabora.com/news-and-blog/news-and-events/nvk-reaches-vulkan-conformance.html>, <https://www.phoronix.com/news/Vulkan-1.4-NVK-Blackwell>
31. Honeykrisp: <https://www.gamingonlinux.com/2024/06/honeykrisp-is-a-new-conformant-linux-vulkan-driver-for-apple-m1>
32. READMEs: `tracel-ai/burn`, `sonos/tract`, `huggingface/candle`, `ggml-org/ggml` (and its `src/` listing).
33. ROS 2 middleware vendors: <https://docs.ros.org/en/kilted/Concepts/Intermediate/About-Different-Middleware-Vendors.html>, <https://docs.ros.org/en/lyrical/Concepts/Intermediate/About-Different-Middleware-Vendors.html>
34. ros2_control, getting started. <https://control.ros.org/rolling/doc/getting_started/getting_started.html>; summary: <https://altair-book.readthedocs.io/en/latest/docs/motion/ros2-control.html>
35. Rapier: <https://docs.rs/rapier2d/0.33.0/src/rapier2d/lib.rs.html>; `crates/` from GitHub, <https://github.com/dimforge/rapier>
36. Erez, Tassa, Todorov, ICRA 2015: <https://homes.cs.washington.edu/%7Etodorov/papers/ErezICRA15.pdf>; 2023 study: <https://ieeexplore.ieee.org/document/10265175>; MuJoCo Warp: <https://mujoco.readthedocs.io/en/3.5.0/mjwarp/index.html>; Genesis critique: <https://hn.jakechampion.name/reader/https://stoneztao.substack.com/p/the-new-hyped-genesis-simulator-is>
37. Franka: <https://frankarobotics.github.io/docs/overview.html>; PREEMPT_RT: <https://wiki.archlinux.org/title/Realtime_kernel_patchset>
38. seL4 MCS. <https://docs.sel4.systems/Tutorials/mcs.html>
