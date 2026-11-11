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

## What changed (defect 1, branch `fix/acpi-power-off`)

The study (`docs/audits/2026-11-11-acpi-power-off-study.md`, skill
`aricodeos-device-study`) compared the sequence with Linux/ACPICA, FreeBSD,
Haiku and the firmware side (coreboot's SMI handler), read the office
machine's AML and the Intel 500 Series PCH datasheet. `\_S5` there is
`{7, 0}` (found correctly by `find_s5`) and `\_PTS(5)` holds nothing that
gates S5 (port-80 debug writes, the TPM's memory-overwrite SMI, Type-C exits
from a D3cold AriCodeOS never enters). The sequence now follows the proven
one:

- `shutdown` first moves the calling thread to the boot CPU
  (`scheduler::move_current_to`; `poweroff.cpu=N` chooses another), then
  turns interrupts off and halts the others, as Linux and FreeBSD do. The
  FADT, `\_S5_` and the chipset facts are gathered before the halt, so the
  sequence afterwards takes neither the ACPI tables' nor the PCI list's
  lock and allocates nothing. The console's lock, which the last lines
  take, is safe: the halt IPI is taken only with interrupts on, so a
  halted CPU holds no interrupt-disabling lock.
- `aricode_acpi::fadt` parses the whole FADT: revision, flags
  (`HW_REDUCED_ACPI`), PM1 event and control blocks and GPE0/GPE1 in their
  legacy and `X_` (GAS) forms, the `X_` one winning as the specification
  says, and the ACPI 5.0 sleep control/status registers. Short or corrupt
  tables are refused.
- `aricode_acpi::sleep::enter_s5`: `ACPI_ENABLE` if `SCI_EN` is clear
  (bounded 3 s), `WAK_STS` cleared, every GPE disabled, every fixed and GPE
  status bit cleared, `SLP_TYP` written alone and then `SLP_TYP | SLP_EN`,
  PM1a before PM1b; after `poweroff.wait_ms` (3000 by default) one more
  write, then the report. Hardware-reduced FADTs use the sleep control
  register. Blocks outside I/O space are refused with the reason; a refusal
  writes nothing.
- The last line says the CPU, the values and registers written, `SCI_EN`
  before and after, what the PM1 status and GPE steps did, how many
  `SLP_EN` writes were made and, on the office machine's chipset
  (`8086:a082`), whether the PCH traps `SLP_EN` into SMM
  (`SMI_EN.SMI_ON_SLP_EN`, datasheet 631120-002 4.2.5).
- `diag` has `power` lines: FADT revision and flags, SCI IRQ, `SMI_CMD`,
  `\_S5_` values, every block with its form and length, and the registers
  now (PM1a control and `SCI_EN`, PM1 status and enable, GPE bits enabled
  and set, `SMI on SLP_EN`).
- Tests: host tests for both modules; `power.acpi_shutdown_from_ap` (4
  CPUs, `poweroff_from=3`, tests only) checks the move to the boot CPU and
  the power-off; `power.poweroff_ignored_desktop` now runs the whole
  sequence without `SLP_EN`; `diag.machine` checks the `power` lines.

After the independent audit (no High finding): an unusable `X_` form falls
back to a valid 32-bit port, a broken PM1b is skipped while PM1a is still
written, PM1 blocks use ACPI's default widths, a FADT with a bad checksum is
used and said, the move to the boot CPU gives up (said) if that CPU's tick
does not advance, `poweroff.*` values out of range are refused and logged,
and `acpidump` saves the FADT too (study, "Follow-up").

Not done, and why: no AML interpreter (`\_PTS` checked by hand for this
firmware), no device quiesce (the veterans' quirks for this family are
about waking right after S5, not an ignored one), no EFI `ResetSystem`
(the kernel cannot call runtime services; Linux would not use it here
either). See the study's sections 5 and 6.

## Next (defect 1)

- With a build of this branch on the stick, the owner runs `diag` on the
  office machine (the `power` lines) and then `shutdown`. If it powers off,
  the incident closes. If not, the photo of the last line says what was
  written, from which CPU, whether the PCH traps `SLP_EN`, and the `diag`
  lines give the FADT's blocks: the next step (device quiesce, or the
  firmware's SMI expectations) starts from them.
- The fix is tested on the ASUS before the office machine.
