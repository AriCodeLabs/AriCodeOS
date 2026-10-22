# Incident 0019: two scenarios depended on host timing under parallel runs

- Opened: 2026-10-23. Status: closed on `feature/parallel-test-runner`
  (ADR 0040): one test expectation corrected, one scenario marked to run
  alone; follow-up below.
- Found by: the parallel test runner's verification, `cargo xtask test
  --all --cpus 4 --jobs 8` inside the development image on the reference
  host (16 CPUs). During that run the container was also limited live to
  10 CPUs (`docker update`), so eight 4-CPU guests (up to 32 vCPU threads
  under TCG) shared 10 host CPUs: well above the limits now documented
  (6 jobs for `--cpus 4`, tests/README.md "Parallel runs"). The thermal
  guard log shows no paused guest during the run.
- Environment: QEMU 10.0 TCG, OVMF, the 0.13.0+ system from `develop`
  d454db5. Neither scenario failed in the runs with `--jobs 8` (1 CPU),
  `--cpus 4 --jobs 4` or `--cpus 4 --jobs 6`.

## What was seen

```text
[FAIL] console.ps2      2.5s  missing '[host] QMP: 10 input events sent'
[FAIL] usb.hid_fault   60.0s  exit None != 33; missing 'ari> shutdown'; missing 'ARICODEOS_SHUTDOWN'; timeout 60s
```

`console.ps2`: the guest printed `console: PS/2 line 'hola' (4 bytes, 10
scancodes)` and `ARICODEOS_KERNEL_READY`; the host's line was `[host] QMP:
failed after 9 events (Connection reset by peer (os error 104))`.

`usb.hid_fault`: the fault, the endpoint recovery and `diag` were all as
expected, then the console showed `ari> shut` and nothing more; the host
had sent all 30 events (`[host] QMP: 30 input events sent`).

## Mechanism

- `console.ps2`: the ten events are h, o, l, a and Enter, each pressed and
  released; the guest's count of 10 scancodes means the tenth (Enter
  released) reached it. The kernel test exits QEMU as soon as the line is
  complete, so QEMU can end before it writes the QMP reply to that last
  `input-send-event`: the runner's QMP client waits for a reply per event
  (`run_input_probe`) and counted 9. The expectation asserted the host's
  tally of replies, which races with a guest-initiated exit by design; a
  slower host only made the race visible. Delivery was never in doubt.
- `usb.hid_fault`: the keys are paced at 30 ms in wall-clock time from the
  marker, so "shutdown" is typed while `diag` is still printing. QEMU's
  usb-kbd queues at most 16 scancodes and drops whole events beyond that
  (`QUEUE_LENGTH`, hw/input/hid.c); the guest drains it at its interrupt
  endpoint's polling rate in guest progress, which slows down when other
  QEMU processes share the host CPUs. With the guest behind, the queue
  overflowed and "down" + Enter were lost, so `shutdown` never ran and the
  scenario timed out. The scenario's own comment already recorded the
  queue limit; the pacing assumes the guest keeps up in wall-clock time.

## Fix

- `console.ps2` asserts the guest's count (it proves delivery) and no
  longer the host's tally; the reason is in the scenario file.
- `usb.hid_fault` gets `serial = true`: the runner starts it before any
  other scenario and runs it with nothing else going, the conditions it was
  written for. The reason is in the scenario file.
- No retry, no longer timeout and no longer pacing: neither change makes a
  failing behaviour pass.

## Follow-up (not done here)

- Typing that waits for the guest instead of the clock: `[host_input]`
  stages that each wait for their own marker (for example the next prompt)
  would let `usb.hid_fault` type `shutdown` only once `diag` has finished,
  and run with the others again.
- `input.virtio` and `gui.two_windows` also assert the host's tally, but
  the guest keeps running after their last event (more checks, the input
  service report), so QMP's reply is not racing an exit there; they passed
  in every parallel run.
