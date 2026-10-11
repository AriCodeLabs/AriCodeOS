# References, capabilities and opportunities for AriCodeOS

Date: 2026-09-28. Base: `4836023e6e4e4afa66125106a18f457107ae70bf`, branch
`fix/audit-4836023`, with local changes in progress.

Comparative study and development proposal; this is not a new fix audit, nor
does it attest that tests were run in this session. The suggested
implementations have not been applied. External sources describe the reference
projects; priorities, designs and acceptance criteria are proposals for
AriCodeOS, not features attributed to those projects.

The main opportunity is to turn the current architecture into a system that can
be used, diagnosed and recovered predictably. Rust, microkernel, capabilities
and WASM all have precedents: differentiation will have to be demonstrated in
user experience, recovery, performance and ease of building applications.

## 1. What we can learn from other projects

| Reference | Documented capabilities | Applicable lesson and limits |
| --- | --- | --- |
| Genode Labs: [Genode/Sculpt](https://genode.org/download/sculpt) | Microkernel, capabilities, isolated drivers, virtual machines and a UI that shows components and their relationships. | Make connections between services and their permissions visible. An administrative view could be used to configure and diagnose AriCodeOS. It does not imply adopting its kernels or its full infrastructure. |
| Google: [Fuchsia, capabilities](https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities) | Components declare what they offer and consume; the environment sets up explicit routes and dependencies. | Evolve the manifest from a list of permissions into contracts between services, with required and optional dependencies. |
| Oxide: [Hubris](https://hubris.oxide.computer/) | Rust, isolated tasks, driver restarts and debugging with Humility, including system inspection and dumps. | Design recovery and diagnostics together with each driver. Its static configuration for microcontrollers does not carry over directly to a dynamic desktop. |
| QNX: [priority inheritance in IPC](https://qnx.com/developers/docs/8.0/com.qnx.doc.neutrino.sys_arch/topic/ipc_Priority_inheritance_messages.html) | Server work receives priorities tied to those of its clients to bound priority inversion. | When AriCodeOS adds priorities, also consider IPC call chains. Changing the scheduler is not enough; nor would it by itself demonstrate hard real time. |
| Ant Group and collaborators: [Asterinas/OSTD](https://www.usenix.org/conference/atc25/presentation/peng-yuke) | Rust framework that separates low-level operations from service code. | Strengthen safe APIs for memory, DMA and MMIO. Its [framekernel](https://asterinas.github.io/book/kernel/the-framekernel-architecture.html) shares an address space: copying that architecture would remove an important property of our current isolation. |
| Community: [Redox OS](https://github.com/redox-os/book/blob/master/src/introduction.md) | General-purpose system in Rust, microkernel, Unix/POSIX oriented. | A reference for studying the effort behind the SDK, tooling and applications. Selective compatibility can ease ports without making POSIX the kernel architecture. |
| Research: [LionsOS/UNSW](https://lionsos.org/) | Components on seL4, separation between drivers and multiplexing, queues and mostly static configuration. | Separate hardware transport, policy and clients. LionsOS describes itself as experimental and does not yet have a complete verification story; it must not be confused with the seL4 kernel proofs. |

Two other cross-cutting references: [WASI](https://wasi.dev/) defines portable
interfaces and an explicit authority model for Wasm applications;
[TUF](https://theupdateframework.io/docs/overview/) addresses authenticity and
freshness of updates, as well as the impact of compromised keys. They help avoid
unnecessary private protocols and complete the update model.

## 2. What we have and what it really means

The local sources consulted are [architecture](../architecture.md),
[master document](../../AriCodeOS.md), [SDK](../sdk.md),
[hardware](../hardware.md), manifests and protocols. "Implemented" here means
present in the code/documentation, not production maturity.

| Area | Existing base | Next step |
| --- | --- | --- |
| Kernel | Rust `no_std`, UEFI, x86-64, protected memory, processes, IPC, SMP. | Per-process resources, performance under load and physical validation. |
| Authority | Handles with rights, revocation, broker, manifests and private app directory. | Per-resource/per-operation permissions, temporary grants, editable policy and explanation of denials. |
| Services and drivers | Ring 3 drivers, supervision from init, virtio, xHCI/HID and VT-d. | Client recovery, channel shutdown, hotplug and policy when DMA isolation is missing. |
| Applications | Rust SDK and own runtime; WASM interpreter with step/depth limits. | Up-front module validation, useful interfaces, compatibility and third-party tooling. |
| Updates | Signed images, A/B slots, attempts and rollback. | Full sequence across boots, power cuts, system health and data migration. |
| Desktop | Compositor, surfaces, focus and input. | Graphical terminal, international text, controlled clipboard and real use with a USB keyboard. |
| Storage and network | Initial AriFS, virtio block, IPv4/DHCP/TCP/DNS/HTTP. | Crash-safe persistence, physical storage, stable networking and TLS. |
| Validation | Host-testable libraries and QEMU scenarios with fault injection. | Long-running tests, hardware, performance regressions and cross-component failures. |

There is local work to filter xHCI, check that input services respond before
publishing them and bound `InputClient` waits, plus HID fixes. It must be
reviewed and tested before closing the
[4836023 audit](2026-09-28-usb-hid-4836023.md). The proposal below generalizes
that work; it does not treat it as absent.

The documentation contains stale states: the README announces 0.1.0, the master
document header 0.0.7 and the architecture doc 0.5.1; some "Known debt" items
contradict features already described. It would be worth keeping a single table
with `proposed / prototype / validated in QEMU / validated on hardware` and
linking its tests. A "completed" phase does not mean its extensions are too.

## 3. Priority improvements and how to implement them

The priorities below express development order, not bug severity. Effort is
relative: small, medium or large; it is not a date estimate.

### A. Common service lifecycle and recovery contract

**Immediate priority; medium/large effort.** Supervision already exists.
Extend it with `Starting`, `Ready`, `Degraded`, `Stopping` and `Failed` states,
startup deadlines, dependencies and a maximum number of restarts with increasing
backoff. A missing optional service should allow continuing in degraded mode.

In [init](../../services/init/src/main.rs), generalize the input mechanism into
a reusable supervisor. Introduce instance identity (a service generation
distinct from the handle generation) and client reconnection. Define how the
device is stopped, IRQ/DMA withdrawn and resources reclaimed before restarting
it. If DMA cannot be stopped with guarantees, keep the device isolated and do
not reuse its buffers prematurely.

**Acceptance:** trigger 100 restarts of a test driver while another service
keeps running; compare memory, handles and mappings before/after. Check both
recovery and exhaustion of the restart limit.

### B. IPC semantics for failures, deadlines and retries

**Immediate priority; medium/large effort.** Timeouts and versioned protocols
already exist. What is missing is turning them into a uniform SDK contract.

In [protocols](../../sdk/proto/src/lib.rs) and clients, add a request
identifier, server generation and total deadline. Define server-side close,
client wakeup and late responses. Cancellation must distinguish "not executed"
from "outcome unknown"; a timeout does not undo an operation. For writes, avoid
automatic retries without a deduplication mechanism or persistent transaction.
For reads, document when a retry is allowed.

**Acceptance:** delay a response until after the timeout and issue another
request; never confuse the two. Restart a server during a write and return a
defined result, without promising exactly-once execution.

### C. Manifests with concrete resources and dependencies

**Next priority; medium/large effort.** Today
[Manifest](../../libs/manifest/src/lib.rs) declares `name`, `caps` and `memory`;
the memory quota refers to DMA buffers. Extend it with a schema version,
required/optional services, rights and independent limits.

Start with a single selected file in read-only mode and network per
service/destination; the broker hands out a restricted endpoint and the server
enforces the restriction. Writing the permission in the manifest is not enough
to make it effective. Keep authorization tied to objects and routes; a client
identity alone does not grant authority. The effective grant will be the
intersection of request, policy and available capabilities. Inspiration: the
Fuchsia routes cited above.

**Acceptance:** two apps receive different objects, one read-only; changing the
policy affects new grants according to the contract, and revoking a grant does
not accidentally revoke someone else's resources. A missing optional dependency
does not prevent startup; required cycles are rejected before launch.

### D. Complete quotas and behaviour under saturation

**Next priority; large effort.** CPU and handles are already pending on the
roadmap. Add limits for IPC queues, processes, shared memory and resources held
by servers on behalf of a client. Define who pays for a transferred buffer and
reserve resources for diagnostics and recovery.

Start with counters and predictable rejection at allocation; then CPU budgets.
If priorities are introduced, study propagation across IPC calls and limits so
an app cannot raise itself indefinitely. QNX provides a precedent, not an
implementation that can be copied without adapting its semantics.

**Acceptance:** an app that saturates CPU, handles or requests hits observable
limits while another keeps making progress within the measured latency target.

### E. Queryable diagnostics, not just serial messages

**Immediate priority for a minimal version; medium effort.** Observability is
already on the roadmap. Implement a bounded per-service status schema: version,
generation, health, last error, restarts, queues, memory and timings. Start with
`services --detail`, `caps` and exporting a report from the shell; the graphical
interface can come later.

Fuchsia offers [Inspect](https://fuchsia.dev/fuchsia-src/development/diagnostics/inspect)
for publishing structured state; Hubris provides the example of inspection and
dumps. For AriCodeOS we propose a ring buffer with request and build
identifiers, with no private file contents or messages by default. Access to
other processes' state will require a diagnostics capability.

**Acceptance:** distinguish "waiting on hardware", "waiting on service" and
"quota exhausted" from a report; disabling the reader neither blocks the
producer nor grows memory without bound. Keeping the build symbols makes it
possible to interpret the dump.

### F. Complete update and recoverable data

**Immediate priority for tests; staged implementation later; large effort.**
A/B and signatures already exist. Add a scenario that keeps the same disk,
installs B, reboots, and confirms B or falls back to A. Inject interruptions at
every write phase and verify data, not just boot messages.

Confirmation must depend on essential services and a read/write operation, not
merely on the kernel being alive. Version app data too: a system rollback does
not automatically revert a migration. Before distributing over the network,
design key rotation and version metadata with TUF as a reference. Separate
authorized recovery from rejection of obsolete versions; any expiry policy needs
a time source and an explicit rule for machines without a reliable clock.

**Acceptance:** every cut leads to a valid image or explicit recovery, without
confirming an incomplete install or destroying previous data. Image signing is
not equivalent to a UEFI Secure Boot chain: the current guide requires
disabling it. The loader and the recovery path are also part of the future
chain of trust.

### G. Drivers and storage for a specific machine

**Immediate planning priority; large effort.** Choose the reference laptop and
record PCI IDs, firmware, storage, USB and IOMMU. Implement the storage
controller that machine needs; AHCI, NVMe and USB mass storage are separate
projects.

The firmware can read the USB stick to boot, but that does not give the kernel
later access to the stick or the disk. USB HID does not provide USB storage
either. For input, add disconnect/reconnect, per-device identification, clearing
of held keys, and later hubs and more general HID descriptors.

**Acceptance:** complete a boot and input session on the chosen machine; test
persistence on a test medium once its driver exists. Publish what works in
QEMU, what works physically and which mode lacks an IOMMU.

### H. WASM useful for third-party applications

**Next priority; large effort.** The interpreter already limits steps, depth and
memory, but does not fully validate the module before running it and its
imported API is minimal. Prioritize a validator, a per-module manifest and
resources granted to the module, separate from the runner's privileges.

Implement files, clock and a small application interface first; select an
explicit WASI profile and check it with external tools. The documentation
consulted already describes WASI 0.3 and the Component Model: do not assume a
current MVP module supports it just by adding a few functions. Compare extending
the interpreter with integrating a compatible runtime after studying `no_std`,
memory, portability, licence and maintenance. JIT and SIMD can wait for metrics.

**Acceptance:** run the same test app on AriCodeOS and a reference runtime using
exactly the declared profile; reject invalid modules and ungranted accesses. A
blocking imported call also needs a deadline: the instruction counter alone does
not bound that time.

## 4. Areas worth adding to or refining in the backlog

This does not claim they are all absent from the project. The "status" column
separates existing plans from details this study proposes to develop.

| Area / status | Useful implementation | How to check the result |
| --- | --- | --- |
| Persistence: initial AriFS | `flush` contract, partial writes, atomic replace and recovery; choose journal or copy-on-write via ADR. | Cuts during data and metadata writes, full disk and detected corruption. A/B does not replace these guarantees. |
| Network: debt already documented | DHCP renewal, link changes, DNS with deadlines, bounded buffers and gradual TCP improvement. TLS as an evaluated library/service. | Loss/reordering, address change and reconnection. HTTPS requires certificate trust, time and entropy. |
| Power: extend roadmap | Monotonic clock, battery, temperature, idle states and suspend/resume with per-driver contracts. | Measure consumption and resume with timers, DMA and input state kept consistent. |
| Desktop: initial GUI | Graphical terminal connected to USB, line editing, Unicode font, Spanish layout, focus and resizing. | Type, edit and save a file without a serial console; test ñ, accents and focus loss. |
| Accessibility: refine design | Scaling, high contrast, keyboard navigation and a semantic tree for future assistive tools. | Complete the main operations without a mouse; avoid a UI that is only pixels with no semantics. |
| Desktop permissions: extend sandbox | File picker that grants access to the chosen file; mediated clipboard, screen capture and global input. | An app without a grant cannot observe other windows; revoking access has a defined effect. |
| ABI and SDK: already planned | Protocol schema, codec generation and a client/server matrix per version. | N/N−1 compatibility when promised; unknown versions fail clearly. [FIDL](https://fuchsia.dev/fuchsia-src/reference/fidl/language/versioning) is a reference, not an automatic ABI guarantee. |
| Packages: already planned | Publisher- and content-based identity, signed manifest, transactional install and separation of data and binaries. | Install, interrupt, update and uninstall without accidental data loss or silent permission expansion. [Fuchsia](https://fuchsia.dev/fuchsia-src/concepts/packages/package) illustrates content-addressed storage. |
| Memory/CPU: extend measurements | IPC latency p50/p95/p99, queues, context switches and time in locks; optimize later with batching and per-CPU queues. | Comparison with the same build, load, CPU and QEMU/KVM mode; do not extrapolate TCG to hardware. |
| Memory sharing: generalize existing base | Separate shared CPU buffers from DMA; typed objects with owner, permissions and mapping lifetime. | Revocation, close, transfer and cleanup after death without use of already freed memory. |
| Guarantees: `unsafe` already has a policy | Inventory of unsafe operations and contracts for MMIO/DMA; start state models for handles, IPC and update. | Property and interleaving tests, review of safe APIs and correspondence with the code. The [seL4 proofs have explicit assumptions](https://sel4.systems/Verification/assumptions.html); unit tests are not formal verification. |
| Entropy and secrets: initial base | Additional entropy sources, an "uninitialized" state, an API that signals unavailability and per-app separated key storage. | Behaviour without RDRAND and without a reliable source; do not present the TSC as a cryptographic guarantee. |
| Build chain: refine operation | Reproducible builds, pinned dependencies, artifact provenance, licence inventory and per-version symbols. | Two controlled builds produce the same unsigned artifact; explain tooling and metadata differences. |
| CI and tests: existing roadmap | Unit/QEMU runner with bounded time and space, evidence retention and per-commit reproduction. | A failure keeps logs and command; it does not exhaust the disk. No `.github/` was found, but that does not rule out external CI. |
| Compatibility: already planned | Port a few applications via SDK/WASM; consider a partial libc or a VM for specific needs. | Matrix of programs that actually work and maintenance cost; postpone full POSIX and Windows. |
| Audio, Wi-Fi and GPU: already considered | Choose hardware and use cases; audio needs deadlines and permissions, Wi-Fi needs reconnection/credentials, GPU needs buffer management. | One reference app and device per subsystem before expanding support. |

## 5. Ideas to differentiate AriCodeOS

These are product proposals, not claims of world-first novelty.

1. **A system that explains a hang.** A command or panel shows the chain
   `app → service → driver`, the awaited resource and the deadline. Requires IPC
   contracts and diagnostics; it would start as a text tool, with restricted
   access.
2. **Permissions for a specific task.** The user selects a document and grants
   an app read access for that session. It relies on a trusted picker and
   restricted endpoints; it does not require granting the whole directory.
3. **Visible, verifiable recovery.** When a driver restarts, the system reports
   which functions recovered and which lost state. A reproducible demo can show
   continuity of other applications without hiding interruptions or promising
   to recover an unknown write.
4. **Diagnostic mode from USB.** A minimal environment inventories hardware,
   explains which drivers are missing and exports a local report. Start with
   screen and serial; exporting to a file will require a working storage driver.
5. **SDK with host execution.** Simulated service implementations let app logic
   be developed and tested on Linux; QEMU checks ABI, isolation and real
   behaviour. It lowers the cost of contributing to the project.
6. **Reproducible system profile.** Describe services, versions, routes and
   limits in a configuration validated at build time. Use the same profile for
   the image and the tests; later visualize it the way Sculpt does.
7. **Per-release health report.** Publish timings, consumption, recovery, tested
   devices and known limits alongside each image. Separating what is
   demonstrated from what is planned would make comparisons much more credible.

## 6. Recommended sequence

| Stage | Deliverable | Dependencies / exit gate |
| --- | --- | --- |
| 0. Consolidate | Review the local USB changes, run regressions and update the capability status. | Audit resolved with evidence; do not infer it from the existence of a patch. |
| 1. Observable recovery | Common service contract, correlated requests, minimal diagnostics and death/restart tests. | No orphaned waits; resources return to the expected level; explicit degraded state. |
| 2. A useful session | Terminal with USB, reference machine and a selected, tested persistence path. | Write and recover a file, shut down/reboot and reproduce the session on the supported machine. |
| 3. Limits and update | Additional quotas, per-resource permissions, write cuts and update across boots. | Another app keeps working under load; rollback preserves compatible data. |
| 4. External applications | Reproducible SDK, documented WASM/WASI profile, packages and controlled compatibility. | An app developed outside the tree is built, installed, run and updated. |
| 5. Optimization and expansion | Performance, power, more devices, audio and additional compatibility. | Each expansion answers a concrete workload or need and has reference hardware/tests. |

Research tracks can overlap; it is not advisable to open new fronts on GPU,
Wi-Fi, browser, JIT, full POSIX and multiple architectures at the same time. The
first proposed demonstrable goal is a useful local session that survives the
failure of a component and can be updated while preserving its data.

Each implementation should land through its own branch, plus an ADR when it
changes a contract, following [WORKFLOW](../WORKFLOW.md). This study does not
change the official roadmap nor treat any proposal as implemented.
