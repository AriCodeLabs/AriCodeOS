# Performance audit evidence

Snapshot: `731c398` (2026-10-23). These probes do not change scheduling,
network service, graphics rendering, allocation or filesystem algorithms.
`guest-instrumentation.patch` adds observations only to the benchmark app:
network metadata RPC, quiet-interval idle counters and a deliberately busy
500 ms interval followed by a 200 ms sleep. It also grants that app network
access. The endpoint order follows `services/init` (system, fs, net, HW).

The two audit-only scenarios run with one and four vCPUs, serially, with
kernel self-tests disabled. Each launches the app twice; the second is the
reported warmed sample. The normal `perf.bench` scenario is unchanged.

## Reproduce the guest observations

Use a clean, isolated worktree at `731c398` and run one QEMU suite on the
host at a time. The original snapshot was not modified by the audit.

```sh
git apply docs/audits/731c398-performance/guest-instrumentation.patch
cp docs/audits/731c398-performance/perf_audit_1.toml tests/qemu/
cp docs/audits/731c398-performance/perf_audit_4.toml tests/qemu/
CARGO_BUILD_JOBS=2 CARGO_NET_OFFLINE=true cargo xtask test --all --area auditperf --jobs 1
```

The patch is audit instrumentation, not a proposed production fix. Its
benchmarks preserve the existing use of `rdtsc`, so the original limitations
of that timer measurement apply. The busy/sleep control prints raw cycles
as well as ratios. On one CPU there can be no guest idle time while the
app remains runnable in its 500 ms busy loop. A ratio much larger than the
remaining fraction of the measured interval identifies bad accounting.
On four CPUs, one busy app does not occupy all four CPUs; apply the control
to aggregate counters, allowing for asynchronous per-CPU observations.

## Host probes

```sh
cargo run --offline --release --manifest-path docs/audits/731c398-performance/host/Cargo.toml -- build/esp/aricodeos/initrd.img
cargo run --offline --release --manifest-path docs/audits/731c398-performance/host/Cargo.toml -- --io-only
python3 docs/audits/731c398-performance/elf_inventory.py target/x86_64-unknown-none/release
```

The host probe uses the production scheduler policy, graphics library and
initrd reader. Scheduler and initrd timings are native host timings,
median of seven batches after 100 warm-up calls. They exclude syscall,
IRQ-lock and context-switch costs and are not guest latency measurements.
The final `host-results.txt` was collected after the guest suite completed
using the final instrumented initrd, during the CI resting interval.
`host-results-preliminary.txt` retains an earlier run against the earlier
image while another QEMU suite was active. CPU frequency and placement
were not pinned; independently timed columns need not add up. Treat the
timings as illustrative scaling evidence, not stable baselines.
The graphics result is a deterministic pixel difference count, not a GPU
benchmark. `current_flip_writes` is derived from the inspected production
loop over the entire back buffer.

The initrd index is a comparison implemented in the probe, not an existing
kernel index. It changes only lookup cost, not process startup as a whole.

The ELF inventory reports real loadable pages, zero-filled storage and the
retained runtime heap symbol. It separates these from debug-rich ELF file
size. Stack columns are the fixed reservations from this snapshot. It
excludes page tables, DMA, device buffers, process metadata and shared
kernel mappings; it is not RSS or a live heap high-water measurement.
The benchmark ELF includes the audit instrumentation. Other service ELFs
are unmodified production sources compiled in this worktree.

## Scheduling interruption

An initial audit invocation waited for the development suite to finish.
The newly enabled `aricodeos-ci` container then began its own cycle while
the audit was running host tests/building. The audit runner was stopped
at the QEMU boundary and its own QEMU process was terminated; the CI was
left running. `guest-suite-interrupted.log` is retained, exit 143. No guest
latencies from that attempt count as measurements. The final invocation
waited specifically for the CI's logged resting interval and passed both
scenarios. This is a new
measurement after removing external interference, not a retry to turn a
failed functional test green.

`--io-only` counts actual AriFS block-device calls over an in-memory disk,
including aligned and unaligned reads and atomic replacement. It checks
the returned data and makes no claim about physical disk latency.

## Summarize guest results

```sh
cp build/scenarios/perf.audit1/serial.log docs/audits/731c398-performance/guest-1cpu.log
cp build/scenarios/perf.audit4/serial.log docs/audits/731c398-performance/guest-4cpu.log
python3 docs/audits/731c398-performance/summarize_guest.py
```

The summary keeps both runs. The report uses the second run. The single-CPU
busy control reproduces idle overcount; the four-CPU aggregate control in
these logs does not. Passing scenarios establish completion, not correctness
of performance counters or compliance with latency budgets.

The final audit worktree restores the benchmark and removes the temporary
scenarios from `tests/qemu`; the patch and copies here are the complete
instrumentation. Build outputs still reflect the instrumented run and must
be rebuilt before using them as ordinary production binaries.
