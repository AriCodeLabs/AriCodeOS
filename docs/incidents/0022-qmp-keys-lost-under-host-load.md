# Incident 0022: keys typed over QMP were lost when the host was loaded

- Opened: 2026-10-23. Status: fixed on `fix/qmp-typing-and-quantum`.
- Found by: the continuous run of `develop` 484da4c in the CI container
  (`tools/docker/ci/`), report
  `2026-10-23T0042Z-develop-484da4c-Silp6x`: `check` and the 1-CPU pass
  green (167 of 167), the `--cpus 4` pass red on `usb.hid_fault` only.
- Environment: QEMU 10.0 TCG, OVMF, the CI container with the default job
  count. `usb.hid_fault` was already marked `serial = true` (incident 0019),
  so the runner started it first in the `--cpus 4` pass with no other
  scenario of that run going.
- Follow-up of incident 0019. A new record and not an extension of 0019:
  0019 is closed, its fix (run alone) is exactly what this failure shows to
  be insufficient, and the fix here is a different one, in the runner,
  that also changes five other scenarios.

## What was seen

```text
[FAIL] usb.hid_fault   60.0s  exit None != 33; missing 'ari> shutdown'; missing 'ARICODEOS_SHUTDOWN'; timeout 60s
```

The end of `failed/test-4cpus/usb.hid_fault/serial.log`: the fault, the
endpoint recovery and `diag` as expected, then

```text
ari> shutown
unknown command: shutown (help)
ari>
[host] QMP: 30 input events sent
```

The host sent all 30 events; the guest received "shutown": the second
'd' of "shutdown" was lost, and the shell ran a command that does not
exist, so the run never ended.

Host conditions: the pass started at about 00:47:51 UTC (02:47:51 local).
The thermal guard log shows no paused guest (`paused=0`) and a 1-minute
load average between 5.4 and 2.4 during the scenario's 60 s, falling from
the end of the 1-CPU pass while other work (agent builds on the host,
outside the CI container) went on. The scenario ran alone in its suite,
but not alone on the machine.

## Mechanism

The runner typed every key on a fixed wall-clock rhythm (`key_delay_ms =
30` after each event) from the moment it saw the marker
(`run_input_probe`, tools/xtask/src/qemu.rs). QEMU's usb-kbd holds at most
16 key codes and drops events beyond that (`QUEUE_LENGTH`,
hw/input/hid.c); the guest drains the queue at its interrupt endpoint's
polling rate, in guest progress. "shutdown" was typed while `diag` was
still printing; when the guest falls behind (here a 4-vCPU TCG guest on a
host busy with other work), queued keys pile up and the device drops what
does not fit. Running the scenario alone within the suite (incident 0019)
removes only the load of the suite's own guests; any other load on the
host recreates the failure. The same assumption exists for every scenario
that types through QMP: the i8042 also queues 16 bytes and drops the rest
(`PS2_QUEUE_SIZE`, hw/input/ps2.c), and virtio-input drops events when the
guest has no buffer posted (hw/input/virtio-input.c).

The cause is the test's, not the system's: a real keyboard user waits for
the screen, and no real device is typed into at a rhythm that ignores
whether the machine keeps up.

## Fix

- Typing follows the guest (tools/xtask/src/typing.rs). A scenario whose
  guest echoes what it reads declares `echo_prompt` in `[host_input]`: the
  runner waits for that prompt before the first key of each line and for
  each key's echo on serial before sending the next one, so at most one
  key is queued in the device whatever the host load. `key_delay_ms`
  remains, as a minimum pause only. Each wait is bounded (10 s,
  `ECHO_TIMEOUT`) and stops at once when QEMU exits; on a timeout the
  runner stops typing and says which key had no echo (`[host] QMP: failed
  after N events (no echo of 'd' within 10 s)`), so a lost key names
  itself instead of producing a wrong command.
- Matching: a cursor in the serial capture that only moves forward. The
  prompt step keeps the previous command's output (`diag` contains every
  letter) from passing for an echo; between the prompt and the echo, other
  output can at worst let one key go early, never block, because the
  earliest occurrence is taken.
- Keys whose echo is not known without the guest's layout or that print
  nothing (`shift+dot`, `shift`) are paced only; letters, digits, space and
  Enter are waited for.
- Scenarios: `echo_prompt = "ari> "` in `usb.hid_fault`,
  `shell.usb_keyboard`, `shell.usb_keyboard_msi` (USB keyboard only) and
  `shell.virtio_keyboard` (virtio). The PS/2 self-test (`test=console_ps2`)
  now echoes each byte like a console and `console.ps2` and
  `console.ps2_es` wait with `echo_prompt = "CONSOLE_PS2_READY"`.
- `usb.hid_fault` types a Shift press first instead of Enter: the fault is
  injected on the first HID report, and a key that prints nothing needs no
  echo (an Enter lost by design would have had to be waited for and would
  time out).
- `usb.hid_fault` no longer has `serial = true`: the only reason was the
  wall-clock pacing, and with echo waits the number of keys in the usb-kbd
  queue no longer depends on how fast the guest runs, so other guests on
  the host cannot make it overflow.
- A host test (`repo_scenarios_typing_more_than_a_device_queue_wait_for_echoes`)
  requires `echo_prompt` from every scenario that types more than 16 key
  events.

Not changed: `usb.hid`, `usb.ehci_hid`, `input.virtio` and
`gui.two_windows` type at most 10 key events, which fit in every device
queue even if the guest drains nothing until the end, and their guest
(`input_client`, the compositor) prints the line only once complete, so
there is no per-key echo to follow. `strict.usb_hid_halt` types a single
Enter that the fault consumes by design.

## Tests

- Host: `typing::tests` (echo mapping, plan with prompt and echo steps,
  forward-only cursor, previous command output not taken for an echo,
  waits on a growing capture, bounded wait naming the lost key, stop on
  QEMU exit) and the scenario rule above.
- QEMU, development container (`--cpus 10 --memory 12g`), branch rebased
  on `develop` adc43a0: `cargo xtask test --all --area
  usb,shell,console,scheduler,smp,input` 100 of 100; full suite `--all`
  170 of 170 and `--all --cpus 4` 170 of 170, `usb.hid_fault` running in
  parallel with the others. The `--cpus 4` pass overlapped the start of
  a CI container run on the same host, so `usb.hid_fault` passed with the
  host loaded beyond the documented limits. Thermal guard: maximum 87 °C,
  no guest paused.
