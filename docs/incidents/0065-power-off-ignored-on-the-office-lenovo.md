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
`shutdown` powers the machine off (`power.acpi_shutdown`).

Two defects, one of them ours whatever the firmware does:

1. **The power stays on.** Either the kernel wrote `SLP_TYP | SLP_EN` to the FADT's PM1
   control blocks and the firmware ignored it, or it never wrote: `acpi_poweroff` returned
   silently without a FADT, without a `\_S5_` package, or with a PM1a block that is not an
   I/O port (`parse_fadt` reads neither `X_PM1a_CNT_BLK`, nor the hardware-reduced flag,
   nor `SLEEP_CONTROL_REG`), and the same line was printed in every case. Which one is not
   known.
2. **Under the desktop the last line was not seen.** While the compositor holds the screen
   the kernel console writes to serial only (ADR 0074 phase B), so the line never reached
   the screen. And nothing was stopped: the other CPUs went on running processes, and the
   halting CPU's `hlt` with interrupts on let the scheduler preempt it too. The Terminal's
   shell was blocked in its call while the rest of the desktop most likely went on: the
   owner saw a machine that did nothing.

## What is not known

Why this machine stays on. Differences with Linux, which powers this model off
(`kernel_power_off`, `acpi_power_off_prepare`, `acpi_enter_sleep_state(ACPI_STATE_S5)`,
`acpi_hw_legacy_sleep`), and FreeBSD (`kern_reboot`, `acpi_shutdown_final`), that may
matter:

- Linux runs `\_PTS(5)` (and `\_TTS`) before the write; AriCodeOS has no AML interpreter
  (`docs/architecture.md`, "no AML") and runs neither. Some firmware arms the power-off in
  `_PTS` (embedded controller state, SMI handlers trapping `SLP_EN`).
- Linux clears the wake status (`WAK_STS`), disables the GPEs and clears their status
  before the write, and writes `SLP_TYP` first, then `SLP_TYP | SLP_EN`; PM1a before
  PM1b, where AriCodeOS writes PM1b first.
- Linux moves to the boot CPU first (`migrate_to_reboot_cpu`); FreeBSD binds to
  `CPU_FIRST()` with the comment "Some systems don't shutdown properly (i.e., ACPI power
  off) if we run on another processor". AriCodeOS writes from whatever CPU ran the call,
  having just halted the others, the boot CPU maybe among them.
- Linux quiesces the controllers (`device_shutdown`, the xHCI among them) first.
- Linux falls back to the EFI `ResetSystem` power-off when there is no usable S5.
- The FADT's extended (`X_`) blocks and the hardware-reduced sleep registers are not read.

No guessed fix for this part (ADR 0028): the cause comes from the firmware's own
description of what it expects, and the new last line (below) on that machine.

## What changed (defect 2)

- `shutdown` turns interrupts off on its CPU and halts the other CPUs before the S5 write,
  waiting (bounded) until they say they stopped (`smp::others_halted`): no process runs
  any more. This matches Linux's `native_machine_shutdown`, not its move to the boot CPU
  (above).
- `acpi_poweroff` says what it did (`power::PowerOff`): no S5, no PM1a block, or the write
  made with its values and whether `SCI_EN` was set. If the power stays on,
  `console::last_words` takes the screen back from the compositor, or takes it under
  `gui`, paints the console's last lines whole (with the strips past the text grid, which
  no cell covers and kept the desktop's dock at 1080 lines), and says one of:

      AriCodeOS: ACPI power-off had no effect (S5 type A/B written to PM1a X, PM1b Y, SCI_EN Z); halting (it is safe to switch off)
      AriCodeOS: no ACPI power-off here (no FADT or \_S5_); halting (it is safe to switch off)
      AriCodeOS: no ACPI power-off here (PM1a control block X); halting (it is safe to switch off)

  so a photo of the screen is the report defect 1 lacked.
- The console waits a bounded time for its lock there: a CPU that panicked holding it is
  halted by nobody, and the line then goes to serial only.
- `poweroff_ignored` (tests only) skips the S5 write; the scenario
  `power.poweroff_ignored_desktop` boots the desktop from a stick on two CPUs as the laptop
  does, shuts down from the serial shell and checks the order (the other CPU halted, the
  skipped write, the last lines on the screen), then the same under `gui`.
  `power.acpi_shutdown` runs on two CPUs too. The pixels are not compared in QEMU; the
  repaint after a reattach, margins included, is host-tested (`aricode-fb-console`).

Left as it is: if the compositor's CPU is halted between the two halves of
`screen_take`, the surface it was taking is on that CPU's stack and the last lines go to
serial only (no hang). The window is a few instructions at the desktop's start.

## Next (defect 1)

- With this build on the stick, the owner runs `shutdown` on the office machine and
  photographs the last line: it says whether the write was made, and with what.
- The `acpi.bin` saved on the office machine (DSDT and SSDTs) is decompiled with `iasl` on
  the build host: `\_S5_`, `\_PTS`, `\_TTS` and what they touch (EC fields, SMI ports,
  GPIO) say what this firmware expects before the write.
- The fix follows the study (skill `aricodeos-device-study`), compared with Linux,
  FreeBSD and Haiku, and is tested on the ASUS before the office machine.
