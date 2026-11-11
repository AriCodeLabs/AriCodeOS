# ACPI power-off: device study (2026-11-11)

Why: on the owner's office laptop (Lenovo V15 G2 ITL, type 82KB, Tiger
Lake) `shutdown` left the machine on, with `ACPI power-off had no effect`
(incident 0065). On the 2012 reference laptop (ASUS, Ivy Bridge) and in
QEMU the same code powers off. Skill followed: `aricodeos-device-study`.
Fix branch: `fix/acpi-power-off`.

## 1. The device

- "The device" is the platform's ACPI fixed hardware: the PM1 event and
  control registers, the GPE0 block and, on Intel, the PMC behind them. On
  the office machine the chipset is the Intel 500 Series On-Package PCH
  (Tiger Lake UP3): its eSPI bridge at `00:1f.0` is `8086:a082` (the `diag`
  the owner saved with dev build 68). The PMC itself (`00:1f.2`) is hidden,
  as usual on this generation, and does not enumerate.
- Who else is involved: the firmware (Insyde-style tables, OEM ID
  `LENOVO CB-01`), its SMI handlers (the PCH can trap the sleep write into
  SMM, section 2), the embedded controller (`EC0_`, `PNP0C09`) that cuts the
  rails when the PCH drives `SLP_S5#`, and the ACPI tables.
- The firmware's description, from the DSDT and SSDTs the owner saved with
  `acpidump` (decompiled with `iasl` on the build host; the dump stays
  outside the repository, only the lines needed are quoted):
  - `\_S5` is a plain root-scope package: `Name (_S5, Package (0x04) {
    0x07, Zero, Zero, Zero })`, AML bytes `08 5F 53 35 5F 12 07 04 0A 07 00
    00 00`. `aricode_acpi::find_s5` reads it as `SLP_TYPa 7, SLP_TYPb 0`
    (checked against the bytes). 7 is "Soft Off" in the PCH's `SLP_TYP`
    table (section 2). No SSDT declares `_S5_`.
  - `SS3` is `Zero` (no `\_S3`: an S0ix "modern standby" machine), `SS4`
    `One`.
  - No `\_TTS`, no `\_SI._SST`.
  - `\_PTS (Arg0)` (DSDT) does, in order: `D8XH` and `ADBG` (writes to the
    port-80 debug register `P80B` and a debug serial buffer), an S3-only
    branch, `\_SB.TPM.PTS (Arg0)` if present, and, if the Type-C xHCI
    exists, `TCON` (if `TRTD`) and `TG0N`/`TG1N` (if `ITRT`).
  - `\_SB.TPM.PTS` (SSDT 06): for `Arg0` 4 or 5, unless `MORD` bit 4 is
    set, writes `MCIP = 2` and `MCIN` to port `0xB2`: a software SMI for the
    TCG Memory Overwrite Request handshake on an orderly shutdown. It
    affects whether the next boot scrubs memory, not whether this one
    powers off.
  - `TCON`/`TG0N`/`TG1N` (SSDT `TcssSsdt`) bring the Type-C subsystem and
    the Thunderbolt DMA controllers out of D3cold before the transition.
    AriCodeOS never puts them in D3cold, so they are already on.
  - So nothing in `\_PTS(5)` gates S5 on this firmware: not running it
    (AriCodeOS has no AML interpreter) is not the cause.
  - The PMC I/O region (`PMIO` at `PMBS`, 0x80 bytes) puts GPE0's status
    at +0x60 and enables at +0x70, as the datasheet does. `PMBS` itself
    lives in the firmware's NVS area, not in the tables.
- No FADT was dumped (`acpidump` saves only the DSDT and SSDTs), so its
  PM1/GPE addresses, its revision and flags are not known yet; `diag` now
  prints them (section 8).

## 2. Official documents

- **ACPI 6.5** (UEFI Forum). Downloading it on 2026-11-11 was refused
  (HTTP 403 from uefi.org to this host, page and PDF alike; not worked
  around). The sections the code cites are the ones ACPICA's comments and
  the earlier code already carry; read them against the PDF when it is at
  hand: 4.8.3.1 PM1 event grouping (status then enable, each half the
  block; `WAK_STS` bit 15, write one to clear), 4.8.3.2 PM1 control
  (`SCI_EN` bit 0, `GBL_RLS` bit 2 write-only, `SLP_TYPx` bits 10-12,
  `SLP_EN` bit 13 write-only), 4.8.3.7 sleep control and status registers
  (hardware-reduced: `SLP_TYP` bits 2-4, `SLP_EN` bit 5, `WAK_STS` bit 7),
  4.8.4.1 GPE register blocks (status half then enable half, byte access),
  5.2.3.2 Generic Address Structure, 5.2.9 FADT (field offsets; "the
  64-bit X_ address supersedes the 32-bit one"; `HW_REDUCED_ACPI` flag bit
  20), 7.4.2 `\_Sx`, 16.1 sleeping states (S5 soft off).
- **Intel 500 Series Chipset Family On-Package PCH Datasheet, Volume 2**,
  631120-002 (February 2023), the office machine's chipset. Read sections:
  - 4.2 PMC I/O registers: PM1 status/enable at ABASE+0, PM1_CNT at +4,
    SMI_EN at +0x30, GPE0 status (128 bits) at +0x60, enables at +0x70.
  - 4.2.1 PM1_EN_STS: `WAK_STS` is set when an enabled wake event occurs
    in a sleep state and moves the system to ON; `PWRBTN_STS` is always a
    wake event; `PCIEXP_WAKE_STS` "does not itself cause a wake event or
    prevent entry to a sleeping state".
  - 4.2.2 PM1_CNT: `SLP_EN` write-only; `SLP_TYP` 111b is Soft Off (S5);
    the `SLP_TYP` bits live in the RTC well.
  - 4.2.5 SMI_EN, bit 4 `SMI_ON_SLP_EN` (this datasheet's name; older
    PCH datasheets and coreboot call it `SLP_SMI_EN`): when set, a write that sets
    `SLP_EN` raises an SMI# **and the PCH does not enter the sleep state**;
    the SMI handler is expected to clear the bit and write `SLP_EN` itself.
    Bit 0 `GBL_SMI_EN` gates every SMI.
  - Intel's licence forbids derivative works: the code cites the section
    and describes the bits in its own words.

## 3. The veterans (source read, nothing built, GPL not copied)

Linux v6.12 (`kernel/reboot.c`, `arch/x86/kernel/reboot.c`,
`drivers/acpi/sleep.c`, `drivers/firmware/efi/reboot.c`,
`arch/x86/platform/efi/quirks.c`, `drivers/usb/host/xhci-pci.c`), ACPICA
as shipped in Linux (`hwsleep.c`, `hwesleep.c`, `hwxfsleep.c`,
`hwregs.c`, `hwgpe.c`, `tbfadt.c`, `aclocal.h`; dual BSD-3-Clause / GPL-2.0),
FreeBSD (`sys/kern/kern_shutdown.c`, `sys/dev/acpica/acpi.c`, BSD), Haiku
(`arch_cpu.cpp`, `bus_managers/acpi/BusManager.cpp`, MIT) and, for the
firmware's side, coreboot's Intel SMI handler
(`soc/intel/common/block/smm/smihandler.c`, GPL-2.0: read only).

| Step | Linux + ACPICA | FreeBSD | Haiku | AriCodeOS before | AriCodeOS now |
|---|---|---|---|---|---|
| Quiesce devices | `device_shutdown()` (each driver's `shutdown`) | `shutdown_*` event handlers | none | none | none (section 5) |
| CPU | `migrate_to_reboot_cpu()`: bound to `reboot_cpu` (0) | `sched_bind(CPU_FIRST())`: "Some systems don't shutdown properly (i.e., ACPI power off) if we run on another processor"; `acpi_shutdown_final` repeats "Some chipsets do not power off the system correctly if called from an AP" | no binding | any CPU, boot CPU possibly halted | moved to CPU 0 (`poweroff.cpu`) |
| Other CPUs | `native_machine_shutdown`: IRQs off, `stop_other_cpus()` (IPI, then NMI) | not stopped before `shutdown_final` (as read; `stop_cpus_hard` is on the panic path) | not stopped | IPI halt, counted | same |
| `_PTS(5)`, `_SST` | `acpi_enter_sleep_state_prep` | same (ACPICA) | same (ACPICA) | none (no AML) | none: checked harmless here (section 1) |
| ACPI mode | at boot (`acpi_enable`, waits 3 s for `SCI_EN`) | at boot | at boot | at the power-off, if `SCI_EN` is 0 | same, bounded 3 s as ACPICA |
| `WAK_STS` | cleared first | ACPICA | ACPICA | no | cleared |
| GPEs | `acpi_power_off_prepare` and `acpi_hw_legacy_sleep` call `acpi_hw_disable_all_gpes()`; then `acpi_hw_clear_acpi_status()` writes every fixed status bit and every GPE status byte; then `acpi_hw_enable_all_wakeup_gpes()` | ACPICA | ACPICA | untouched | all disabled, all status cleared (no `_PRW` wake devices kept) |
| Address forms | `X_` GAS when non-zero, else the 32-bit field (`acpi_tb_convert_fadt`); `acpi_force_32bit_fadt_addr` for corrupt 64-bit ones | ACPICA | ACPICA | 32-bit fields only | `X_` when non-zero and usable as I/O ports (I/O space, bit offset 0, below 0x10000), else a valid 32-bit field, said in `diag`; mismatch reported |
| Widths | PM1 event 32 bits, PM1 control 16 (`acpi_gbl_use_default_register_widths`), GPE: the legacy byte length (`acpi_ev_gpe_initialize`) | ACPICA | ACPICA | none | the same; a differing FADT length is said (`FADT says N`) |
| FADT checksum | warns, uses the table | ACPICA | ACPICA | refused the table | used, said at boot and in `diag` |
| Write | `SLP_TYP` alone, then `SLP_TYP | SLP_EN` ("to workaround poorly implemented hardware"), PM1a then PM1b | ACPICA | ACPICA | one write, PM1b before PM1a | split, PM1a then PM1b |
| Cache flush | only below S4 | | | | none (S5) |
| If still on | waits 10 s, writes once more, then spins on `WAK_STS` forever | waits 1 s, prints "Power-off failed! - timeout" | `panic()` | ~10M spins, last line | waits `poweroff.wait_ms` (3 s), writes once more, waits, last line |
| HW-reduced | `acpi_hw_extended_sleep`: `WAK_STS` in sleep status, `SLP_TYPa | SLP_EN` in sleep control | ACPICA | ACPICA | not read | same, I/O space only |
| EFI | `ResetSystem(EfiResetShutdown)` first only if HW-reduced or no S5 (`efi_poweroff_required`) | not read | no | none | none (section 6) |

Differences with the veterans that were kept, and why:

- ACPICA's retry after ten seconds writes the PM1 control register with
  only `SLP_EN` and the preserved bits, which leaves `SLP_TYP` at 0 (S0) on
  hardware that takes the field from the write. AriCodeOS retries with the
  full `SLP_TYP | SLP_EN` value.
- ACPICA keeps the fixed-event enables (power button) as the OS set them;
  so does AriCodeOS (it never changes them).
- The firmware's side (coreboot, read only): its sleep SMI handler clears
  `SLP_SMI_EN`, reads `SLP_TYP` back from PM1_CNT (so the trapped write does
  store `SLP_TYP`), runs board hooks, for S5 disables every GPE, turns bus
  mastering off on bus 0, clears PM1 status, and writes `SLP_EN` again. A
  closed firmware may skip some of that and rely on the OS having done it.

## 4. The real hardware (read only)

- The office machine is not reachable from the build host. What is known:
  `diag` from dev build 68 (chipset `8086:a082`, 4 CPUs) and its AML.
- The old last line could not tell the cases apart: it was the same
  whether the write was made or not, and did not say which CPU, which
  address form, or whether the PCH traps `SLP_EN`.
- The new `diag` `power` lines (section 8) read, without writing:
  the FADT's facts, PM1a control (`SCI_EN`), PM1 status and enable, how many
  GPE0 bits are enabled and set, and on `8086:a082` `SMI_EN` bit 4
  (`SMI_ON_SLP_EN`) and bit 0. Reading these registers has no side effect
  (status bits clear on a write of one; `SMI_EN` is plain read/write).

## 5. Resources

| Resource | Offered | Used now | Why |
|---|---|---|---|
| PM1 control (legacy or `X_`) | S5 entry | yes | the power-off |
| PM1 event block | wake status, fixed events | status cleared, enables kept | ACPICA's order |
| GPE0/GPE1 | wake events | all disabled and cleared | no wake device kept: an AML-less kernel knows no `_PRW` |
| Sleep control/status (ACPI 5) | HW-reduced S5 | yes, I/O space | memory-space registers refused with the reason (not seen on any machine yet) |
| `SMI_EN` (Intel) | says whether firmware traps the sleep | read for `diag` and the last line | never written: the firmware's handler owns the sleep then |
| Device quiesce | driver `shutdown` hooks | none | the drivers are user processes; none of Linux's xHCI shutdown quirks for this family concerns an ignored S5 (`XHCI_SPURIOUS_REBOOT`/`_WAKEUP` are Haswell-era wake-after-S5, `XHCI_RESET_TO_DEFAULT` on Tiger Lake is a boot-delay fix). A future step if the office machine still stays on |
| EFI `ResetSystem` | firmware power-off | no | not reachable (section 6) |

Measurements: none to take on QEMU (it powers off on the first write); the
office machine's next `diag` and last line are the measurement.

## 6. Fail safe

- Every block the sequence needs is checked before the first write: PM1a
  control present, in I/O space, at bit offset 0, inside the port range; a
  PM1a control reading 0xFFFF (nothing answers) refuses too. A refusal
  writes nothing and the last line names the reason. An unusable `X_` form
  with a valid 32-bit field falls back to the 32-bit port first (said).
- A declared PM1b that is not usable, or reads 0xFFFF, is skipped and
  named, and PM1a is written all the same, as ACPICA does. Optional blocks
  (PM1 event, GPE0, GPE1) that are declared but unusable (memory space,
  bit offset, odd length, past the port range) are skipped and named.
- Access widths: 16 bits for PM1 registers, 8 for GPE and sleep registers,
  whatever the GAS's access size says (ACPICA's default widths); the access
  size is not checked.
- EFI `ResetSystem(EfiResetShutdown)`: AriBoot calls `ExitBootServices` and
  passes neither the system table nor a virtual address map to the kernel,
  which never calls runtime services (no ADR covers them). Linux would not
  use it on this Lenovo either (`efi_poweroff_required` is true only when
  the platform is HW-reduced or has no `\_S5_`). Left as a future option:
  it needs the loader to pass the runtime services table and the kernel to
  map their regions, a design of its own.
- If the power stays on, the machine halts with the last line on screen,
  as before.

## 7. Security

- FADT values are untrusted (ADR 0051): ports are bounded below 0x10000,
  lengths checked, memory-space addresses refused; no physical address from
  the FADT is mapped or written.
- `SMI_EN` is read only on a chipset in the table (`8086:a082`), at the
  ACPI base from the FADT; nothing else is probed.
- No new authority: `system_power` keeps its `MANAGE` right check.

## 8. Tests and record

- `aricode_acpi::fadt` (host): legacy-only and ACPI 6 tables, `X_` winning
  with the mismatch said, lengths from the bit width, HW-reduced flag and
  sleep registers, short/truncated/corrupt/mislabelled tables refused, port
  bounds.
- `aricode_acpi::sleep` (host, a fake register bank from ACPI and the PCH
  datasheet): the exact order of writes (`WAK_STS`, GPE enables, PM1 and GPE
  status, `SLP_TYP` alone, `SLP_TYP | SLP_EN` twice with the wait), PM1a
  before PM1b with the OR of both read, `ACPI_ENABLE` with a bounded wait,
  `enter: false` (no `SLP_EN`), every refusal writing nothing, unusable
  optional blocks skipped, the hardware-reduced path.
- QEMU: `power.acpi_shutdown` (2 CPUs) and the new
  `power.acpi_shutdown_from_ap` (4 CPUs, `poweroff_from=3`: the call moves
  to CPU 3 and then back to CPU 0) power off; `power.poweroff_ignored_desktop`
  runs the whole sequence without `SLP_EN` on q35's blocks; `diag.machine`
  checks the `power` lines.
- On the office machine (only it can say): `diag` (the `power` lines) and,
  after `shutdown`, the last line if it stays on.

## Conclusions

Which differences most plausibly explain the office machine, in order:

1. **The write may never have been made.** The old code read only the
   32-bit PM1 fields; a FADT that fills only `X_PM1a_CNT_BLK` (or puts a
   non-I/O address there) gave the same "had no effect" line as a write
   the firmware ignored. Now the `X_` form is read and a refusal says why.
   Likelihood: not high (Intel's reference FADTs fill both forms), but the
   old build cannot exclude it, and the new `diag` settles it at once.
2. **The write came from an application processor with the boot CPU
   halted.** The shell thread runs on any of the four CPUs (three chances
   in four on an AP), and both Linux and FreeBSD say some systems do not
   power off from an AP. On a PCH that traps `SLP_EN` into SMM (section 2,
   `SMI_ON_SLP_EN`), firmware that expects the sleep SMI from the boot CPU
   fails exactly this way. Fixed: the call moves to CPU 0 first.
3. **The firmware's sleep SMI handler found what an ACPI OS leaves done
   undone**: GPEs enabled or pending, `WAK_STS` set, a single combined
   write. Linux does all of that before the write; AriCodeOS did none.
   Fixed: ACPICA's order.
4. `\_PTS(5)` not run: checked against this firmware's AML; nothing there
   gates S5. Not the cause.
5. Devices not quiesced (xHCI, IOMMU): the veterans' quirks for this
   family concern a wake right after S5 (a reboot), not an ignored one. Not
   done; next step only if the machine still stays on.

What only the office machine can confirm: whether the write is now made
(the last line), on which CPU, with which addresses and form, whether the
PCH traps `SLP_EN` (`SMI on SLP_EN`), `SCI_EN` before and after, and
whether this build powers it off.

## Follow-up after the independent audit (2026-11-11)

The audit of `5c619dd` found no High finding and judged the register
semantics to match ACPICA. Its Medium finding was that the first version
refused more than ACPICA and the old code did (an unusable `X_` form with a
good 32-bit field, a broken PM1b, a zero PM1 control length): those now
fall back, skip or take ACPI's default width, as in the table above. The
Low ones: a bounded move to the boot CPU (its tick must advance within ten
ticks, or the power-off runs where it was called, said), a FADT with a bad
checksum used and said, saturating waits, `poweroff.wait_ms` 500 to 60000
with refusals logged, a bit offset refused, `read8` removed, a prepared
report no longer claiming `SLP_EN`, more host tests (masking of read-back
write-only bits, a floating PM1b, every FADT length from 116 to 280).
`acpidump` now saves the FADT too, so the next dump from the office
machine carries its blocks.

## Sources

- Intel 500 Series Chipset Family On-Package PCH Datasheet Vol. 2,
  631120-002, via <https://cdrdv2.intel.com/v1/dl/getContent/631120>.
- Linux v6.12 and ACPICA (in Linux), <https://github.com/torvalds/linux>.
- FreeBSD, <https://github.com/freebsd/freebsd-src>.
- Haiku, <https://github.com/haiku/haiku>.
- coreboot, <https://github.com/coreboot/coreboot> (GPL: read only).
- ACPI 6.5, <https://uefi.org/specs/ACPI/6.5/> (not downloadable from this
  host on 2026-11-11).
