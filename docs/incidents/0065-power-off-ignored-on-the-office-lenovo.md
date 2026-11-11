# Incident 0065: the power-off ignored on the office Lenovo

- Date: 2026-11-11
- Environment: the owner's office machine, a Lenovo V15 G2 ITL (type 82KB, Tiger Lake),
  booted from the dev build 67 stick, desktop session.
- Work branch: `fix/shutdown-screen`, from `develop` at `823a92a`.

## What was seen

The owner, in their words: `shutdown` in the desktop's Terminal "se queda parado y no hace
nada" (it stays still and does nothing); closed another way, the machine stopped with

    AriCodeOS: ACPI power-off had no effect; halting (it is safe to switch off)

and had to be switched off by its power button. On the reference laptop (ASUS) and in QEMU
the same S5 write powers the machine off (`power.acpi_shutdown`).

Two defects, one of them ours whatever the firmware does:

1. **The power stays on.** The kernel writes `SLP_TYP | SLP_EN` to the FADT's PM1a (and
   PM1b) control block with the sleep type of the DSDT's `\_S5_` package, and nothing
   happens. Why is not known (below).
2. **Under the desktop the last line is not seen.** While the compositor holds the screen
   the kernel console writes to serial only (ADR 0074 phase B), so the "safe to switch off"
   line never reached the screen: the desktop's last picture stayed, frozen, and the
   machine looked hung. Worse, the halt after the failed power-off stopped only the CPU
   that ran `shutdown`, with its interrupts left as they were: the other CPUs went on
   running processes, so the system was not even stopped.

## What is not known

Why this firmware ignores the S5 write. Differences with Linux, which powers this model
off (`acpi_enter_sleep_state(ACPI_STATE_S5)`, `acpi_hw_legacy_sleep`), that may matter:

- Linux runs `\_PTS(5)` (and `\_TTS`) before the write; AriCodeOS has no AML interpreter
  (section 31) and runs neither. Some firmware arms the power-off in `_PTS` (embedded
  controller state, SMI handlers trapping `SLP_EN`).
- Linux clears the wake status (`WAK_STS`) and the GPE status and enable registers before
  the write, and writes `SLP_TYP` first, then `SLP_TYP | SLP_EN` in a second write.
- The `\_S5_` values the kernel found on this machine are not in any report: the boot log
  goes to serial, which this laptop does not have.

No guessed fix for this part (ADR 0028): the cause comes from the firmware's own
description of what it expects.

## What changed (defect 2)

- `shutdown` turns interrupts off on its CPU and halts the other CPUs before the S5 write,
  as Linux stops them first (`native_machine_shutdown`): no process runs any more.
- If the power stays on, the console takes the screen back from the compositor
  (`console::take_screen_for_good`) and paints its last lines whole, the "safe to switch
  off" line included: the owner sees why the machine stopped instead of a frozen desktop.
- `poweroff_ignored` (tests only) skips the S5 write as this firmware ignored it; the
  scenario `power.poweroff_ignored_desktop` boots the desktop from a stick as the laptop
  does, shuts down from the serial shell and checks the order: the shutdown, the skipped
  write, the screen taken back, the last line.

## Next (defect 1)

- The `acpi.bin` the owner saved on the office machine (DSDT and SSDTs) is decompiled with
  `iasl` on the build host: `\_S5_`, `\_PTS`, `\_TTS` and what they touch (EC fields,
  SMI ports, GPIO) say what this firmware expects before the write.
- `diag` gains the power-off facts it lacks (the `\_S5_` values, PM1a/PM1b, `SCI_EN`, the
  FADT's hardware-reduced flag), so a report from a machine without serial carries them.
- The fix follows the study (skill `aricodeos-device-study`), compared with Linux,
  FreeBSD and Haiku, and is tested on the ASUS before the office machine.
