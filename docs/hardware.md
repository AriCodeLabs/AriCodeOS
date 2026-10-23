# AriCodeOS on real hardware (Phase 34)

Guide for the first boot on the test laptop. The order and the precautions
follow sections 95 to 97 of the master document: **the internal disk is
never touched**; AriCodeOS boots from and writes only to its own USB stick.

## What you need

- An x86-64 laptop with UEFI firmware (UEFI boot, not CSM/legacy) and
  Secure Boot disabled. VT-d/"Intel Virtualization for Directed I/O" may
  stay enabled: the kernel uses it if the DMAR declares it.
- A USB stick of at least 128 MiB that can be wiped completely.
- Keyboard: the built-in one (PS/2 via i8042) serves as the kernel console.
  USB keyboards and mice (boot HID) are handled by `usbd` on top of the xHCI
  and by `usb_ehci` on the EHCI ports (ADR 0031), and reach the shell
  through the console bridge and the compositor with `gui`.
- Without a serial port, the screen is the diagnostic channel: the kernel
  writes its console to the GOP framebuffer unless `gui` is set.

## Build and write the image

```bash
cargo xtask usb-image                       # build/aricodeos-usb.img, cmdline "shell"
cargo xtask usb-image --cmdline "shell kbd=es"   # Spanish keyboard (`:` with Shift+period, `@` with AltGr)
cargo xtask usb-image --cmdline "shell log=debug"
lsblk                                       # identify the USB stick (e.g. /dev/sdX), NEVER the internal disk
sudo dd if=build/aricodeos-usb.img of=/dev/sdX bs=4M conv=fsync status=progress
```

The image is a GPT disk with six partitions: `ESP` (FAT32 with AriBoot,
kernel, initrd and `cmdline.txt`), `ARI0` (AriFS with only an English
`README`, plus the files `--authorized-key` and `--update-source` add),
`SYSA` (signed system image, slot A active), `SYSB` and `UPDATE` (empty)
and `ARIBOOT` (A/B state). It is the same image that the
`usb.image_boot` scenario boots in QEMU, so what gets written has already
booted once.

## First boot

1. With the laptop powered off, plug in the USB stick and enter the
   firmware boot menu (F12, Esc or F9 depending on the vendor). Choose the
   USB stick in UEFI mode.
2. AriBoot prints its traces on screen (slots, GOP, ACPI, kernel,
   cmdline, memory, pages, `ExitBootServices -> AriKernel`), paints two
   beacons in the top-left corner and jumps to the kernel; the kernel paints
   the third, clears the screen, reports CPU, memory, ACPI, APIC, SMP,
   PCI and IOMMU, and launches init.
3. With `shell` on the cmdline, `ari> ` appears. Useful commands: `cpus`,
   `mem`, `devices`, `ps`, `uptime`, `ls`, `cat`, `write`, `reboot`,
   `shutdown`. The file system is the ARI0 partition of the USB stick
   itself, served by usbd (ADR 0023).

## What a normal boot does to the stick (incident 0005, ADR 0032)

A boot without `selftest` on the cmdline, which is every boot of a
`cargo xtask usb-image` stick, runs only checks that read:

- `hello`, a process killed by a fault, the SPAWNER rights and a driver
  restart (no disk, no network);
- `AriInit: block service answers: N sectors, GPT read, ARI0 at LBA L`;
- `AriInit: file service answers: N files` (a list of AriFS);
- `AriInit: network service has address A`, or `network service: no
  address yet (DHCP pending)`, which is not a failure. On the Realtek this
  is a single request, made after the USB stick and keyboard are up, with
  no wait (at most half a second if netd is slow to answer); `net` in the
  shell shows the address later.

If the block or file check fails, init stops before the shell: `block
service does not answer Info` (exit code 11), `no ARI0 partition on the
disk` (12), `fsd did not start` (13), `file service does not answer a
list` (14).

It creates no test file, writes no test sector and sends nothing to the
QEMU test hosts (10.0.2.2). What the system itself may write at boot:

- the A/B confirmation of a freshly installed slot (`ARIBOOT`);
- fsd's journal recovery at mount, only after a power cut left a replace
  pending or the journal broken (`fsd: pending replace ... redone at
  mount`, `broken journal discarded at mount`);
- sshd, only with `authorized_keys` installed: the host key the first time
  it starts if the stick has none (`usb-image --authorized-key` installs
  one, so normally never; `random_seed` is saved just before), and
  `random_seed`, rewritten on the first accepted SSH connection of each
  boot, not at boot (a failed save says so and is retried on the next
  connection).

The test clients (`fs_client`, `blk_client`, `net_client`, `tcp_client`,
`http_client`, `input_client`) run only with `selftest`, which belongs to
the QEMU scenarios: `cargo xtask usb-image --cmdline` refuses it, like
every other test-only key (`qemu_exit`, `test=`, `usb_fault`, `blk_fault`,
`restart_storm`, `service_hang`, `init_fail_late`, `input_restart`,
`shell_fault`, `gui_trace`) and the keys AriBoot adds itself (`boot_slot`,
`boot_pending`). If a stick's
`cmdline.txt` is edited by hand to carry it, the screen says `AriInit:
selftest: running the test-only boot checks` and the boot stops at
`fs_client exited with 2` before any write, because the stick has none of
the test disk's files. Sticks written before this change carry a
`nuevo.txt`, the test disk's sample files and the `ARIDISK` pattern at the
tail of ARI0; none of them is used any more, and AriFS has no delete yet,
so rewriting the stick with a current image is the way to drop them.

## Status on the test laptop (2026-10-10)

First complete boot achieved after two AriBoot fixes (non-exclusive GOP
and standard memory types, ADR 0021). The machine: 2012 Intel (xHCI
`8086:1e31`, two EHCI), Realtek `10ec:8136` (Ethernet) and `10ec:5286`
(card reader), Atheros `168c:0032` (Wi-Fi), no DMAR.

| Works | Missing |
|---|---|
| AriBoot: slots, GOP 800x600, kernel and initrd verified | Atheros AR9485 Wi-Fi (out of scope: ath9k, 802.11 and WPA2) |
| Wired networking with `rtl_net` (ADR 0024): DHCP, `ping` to the router in 1 ms and `dns google.com` resolved (2026-10-10) | |
| USB stick as a block service over USB (ADR 0023; "USB Disk 3.0", no `SYNCHRONIZE CACHE`), fsd, slot A confirmation and shell over the PS/2 keyboard (2026-10-10) | EHCI driver written (ADR 0031, 2026-10-17), not yet tried on the laptop |
| Kernel: memory, ACPI, APIC, PCI (17 functions), init and its checks, `ARICODEOS_KERNEL_READY` | Power: sleep states and battery |
| PS/2 keyboard on the kernel console (backspace included) | |
| xHCI with MSI (ADR 0022; the controller has no MSI-X), firmware handoff, ports routed, 8 ports, 16 scratchpads, input service ok | |

## EHCI ports (ADR 0031)

The laptop's USB 2.0 ports are wired to the two EHCI controllers through
Intel's rate matching hubs; the switchable ones can also be routed to the
xHCI. usbd routes every port the firmware allows (`USB2PRM`) to the xHCI
(trace `usb-input: Intel ports routed to the xHCI (USB2 m, USB3 m)`); the
rest stay on an EHCI and are served by `usb_ehci`. Expected traces
(illustrative values, not yet seen on the laptop):

```text
usb-ehci: EHCI ready (HCIVERSION 0100, 2 ports, 0 companion controllers, 64-bit addressing, polled)
usb-ehci: port 1: device 8087:0024 class 9 bcdUSB 0200 speed high address 1
usb-ehci: port 1: hub with 6 ports
usb-ehci: port 1.2: device ... speed low address 2 (translator: hub 1 port 2)
usb-ehci: port 1.2: HID keyboard (boot protocol, 8 bytes per report, endpoint 1, every 8 ms)
```

`port 1.2` is port 2 of the hub on root port 1. Devices of other classes
(webcam, Bluetooth, card reader) are listed and left unconfigured. The EHCI
functions have no MSI: the driver polls and needs no IRQ. If a stick on an
EHCI port is the only one, it becomes the system disk (`AriInit: block
service via USB`).

## Diagnostics without photos

The shell saves the full report on the USB stick itself and the host reads
it from the ARI0 partition:

```bash
# on the laptop
ari> diag diag.txt
ari> shutdown
# on the host, with the USB stick attached as /dev/sdX
sudo dd if=/dev/sdX of=build/usb-dump.img bs=1M count=70 status=none
cargo xtask fs-cat build/usb-dump.img            # lists the files in ARI0
cargo xtask fs-cat build/usb-dump.img diag.txt   # uptime, memory, CPUs, entropy, processes, services, PCI and events
```

The 70 MiB cover the ARI0 partition of the USB image (LBA 133120 to 139263).

### Entropy: does the laptop have `rdrand`?

Read it from `diag` (on the console, over SSH, or saved with `diag NAME`
and read on the host with `cargo xtask fs-cat`): its `entropy` line says
`entropy  rdrand` when the kernel generator was seeded from `rdrand`, or
`entropy  TSC + interrupts (N samples, M events): weak` when the CPU has
none (ADR 0039). The fault events `net tcp secret weak x1 (code 2)` and,
when sshd generated a host key with no `random_seed` from an earlier boot,
`ssh entropy weak (code 2)` say the same in the event list while they are
among the recent ones. The kernel's boot line `random: ChaCha20 generator
seeded with rdrand` or `... with TSC (no rdrand: limited entropy)` remains. The laptop's PCH (`8086:1e31`,
7-series) pairs with Ivy Bridge (usually `rdrand`) or Sandy Bridge (none).

**Read on the reference laptop (2026-10-23, version 11):** `entropy  TSC +
interrupts (72 samples, 1737 events): weak`. The laptop's kernel generator
gets no hardware entropy (a Sandy Bridge CPU without `rdrand`, or values
rejected as implausible: the boot line on screen tells which). Consequences:
the SSH host key on its stick was made on the build host
(`usb-image --authorized-key`), so it is not affected; netd's TCP secret
(ADR 0034) and any key generated on the machine rest on TSC and interrupt
timing only, which makes a kernel seed saved across boots (open in ADR 0034
and incident 0014) worth doing for this machine.

**Carried seed on the laptop (2026-10-23, develop 577fa52, stick
reflashed, ADR 0043):** first boot of the fresh stick: `entropy  TSC +
interrupts (70 samples, 223 events): weak, no carried seed (first boot)`,
with `net tcp secret weak x1 (code 2)` and `random seed missing (code 2)`
in diag, as designed. After a reboot over SSH: `entropy  TSC + interrupts +
carried seed`, no weak event, network up. From the second boot on, this
machine's generator starts from the seed of the previous boot.

**Carried seed (ADR 0043).** AriBoot now carries a kernel seed across
boots in sectors 1 and 2 of the stick's `ARIBOOT` partition. A freshly
flashed stick ships none, so its first boot still reads `entropy  TSC +
interrupts (N samples, M events): weak, no carried seed (first boot)` with
`random seed missing (code 2)` among the events, and init writes the first
seed (`AriInit: random seed saved for the next boot (early, generation
1)`, then generation 2 after the boot services). From the second boot on
`diag` should read `entropy  TSC + interrupts + carried seed`, with no
`net tcp secret weak`; AriBoot's screen line is `seed: carried seed of
generation N taken, successor of generation N+1 saved`. Not yet read on the
laptop. `random seed not saved (code 4)` means AriBoot could not write the
stick (the seed was then not used); `random seed unreadable (code 3)` a
damaged store, which init rewrites.
It decides the quality of sshd's keys (ADR 0025) and of netd's TCP secret
(ADR 0034). Not yet read on the laptop (2026-10-22).

## If something fails

The cmdline switches isolate subsystems. Edit them in
`aricodeos/cmdline.txt` on the USB stick's ESP partition (mountable on any
system) or regenerate the image with `--cmdline`:

| Symptom | Try |
|---|---|
| Stops at "slots: booting from slot…" with no further lines | Image older than 0.8.2: AriBoot opened the GOP exclusively and the firmware stopped drawing the console. Rewrite with a current image |
| Stops at "memory: GetMemoryMap (size)" without the reply line | The firmware does not return from `GetMemoryMap`. In images older than 0.8.2 this was caused by AriBoot's custom memory type (ADR 0021); with a current image, note down the `kernel:` and `initrd.img:` lines |
| "GetMemoryMap: invalid response" | The firmware replies outside the specification; note down the `memory: GetMemoryMap ->` line |
| Stops after "ExitBootServices -> AriKernel" | Look at the beacons (below) |
| Hangs after "smp:" or with several CPUs | `smp=off` |
| Black screen or unresponsive devices after "iommu:" | `iommu=off` |
| Too much output | `log=warn`; too little: `log=debug` |
| No keyboard | check that the firmware exposes the i8042 (some disable it with "USB legacy"); the `console: PS/2 keyboard` trace must appear |
| Shift+period types `>` instead of `:` | The keyboard is Spanish and the console uses the US layout: `kbd=es` on the cmdline (`cargo xtask usb-image --cmdline "shell kbd=es"`). Only affects the kernel's PS/2 keyboard; USB and virtio stay on US |
| "usb-input: no BAR 0 or no IRQ among the capabilities" | The function has neither MSI nor MSI-X (the `pci: … claimed (…)` trace says so); INTx is not supported |
| "usb-input: xHCI initialization failed: …" | The step that failed: capabilities, DMA memory (the buffer is sized with the scratchpads the controller requests), reset or start |
| "usb-input: no USB storage" with the USB stick plugged in | The USB stick is not on the xHCI: check the `Intel ports routed to xHCI` trace; if it is missing or the port is not switchable, the stick hangs off an EHCI and `usb-ehci: port 1.N: USB storage ...` should appear instead (see "EHCI ports" below) |
| "usb-ehci: port N: full-speed device on a root port without companion controllers" | A full- or low-speed device reached an EHCI root port directly; on Intel 6/7 series that should not happen (the rate matching hub sits there): note down the line and the `PORTSC` values of a `strict` dump |
| "usb-ehci: ... transfer to address A endpoint E (... speed) failed: ..." | A failed USB transfer on the EHCI with its qTD token (ADR 0028); it is also in `diag` as `ehci ...`. Boot with `strict` to stop at the first one with a dump of the controller |
| "usb-ehci: USB storage unusable until it is plugged in again" | The Bulk-Only Reset Recovery failed after a transfer failure; replug the stick and note down the lines before |
| "usb-input: port N: …" with an INQUIRY, TEST UNIT READY or READ CAPACITY error | The USB stick does not respond to the bulk-only protocol; note down the line |
| "AriInit: no ARI0 partition on the disk" | The USB stick does not carry the `cargo xtask usb-image` image (or something else was written over it) |
| "process: the ELF does not fit in the user region" when launching a program | The binary was not linked into the user region (it lacks a `build.rs` with `aricode_build::user_program`); `cargo xtask build` now detects this before generating the image |
| "rtl-net: no link (cable unplugged?)" | The PHY sees no carrier: the cable, or the firmware did not leave autonegotiation done (rtl-net does not program the PHY yet) |
| "rtl-net: initialization failed: …" | No memory BAR, no DMA, or the card does not complete the reset; note down the `pci: … 10ec:8136 claimed (…)` line |
| `net` in the shell shows no address after a few seconds | DHCP gets no reply: check the link (`rtl-net: link at N Mb/s`) and that the network has a DHCP server |
| "AriInit: block service does not answer Info" or "AriInit: file service does not answer a list" | The storage stack did not come up (init stops, exit code 11 or 14); the `usb-input:` or `fsd:` lines just before say why |
| `write`, `diag NAME` or the A/B confirmation fails, preceded by "usb-input: WRITE(10) … rejected: sense k/asc/ascq" | The USB stick rejects writes; the sense (key/asc/ascq) says why (7/… = write-protected). A normal boot writes no test data (ADR 0032), so this shows at the first real write. A USB stick that does not support `SYNCHRONIZE CACHE` is reported and is not an error |

Note down (or photograph) the last lines before the failure: `EXCEPTION`,
`KERNEL PANIC` and `iommu: DMA fault` carry the necessary information.

### Beacons

Between `ExitBootServices` and the kernel's first line there are no traces
(the firmware is gone and the kernel has no console yet). To see how far
boot gets, AriBoot and the kernel paint 32 px squares on the top row of the
screen (`Beacon` in `boot-protocol`): the first three slots are white,
green and orange; to their right, a cyan square for each loader step after
the cmdline (useful even if the firmware console stops). The kernel console
erases them when it starts, so they are only visible if something stops
earlier.

| Screen | Where it stopped |
|---|---|
| Last line `cmdline:` or one of `memory: GetMemoryMap…`, no cyan squares | The firmware does not respond: to its text console (if the `memory:` line is missing) or to `GetMemoryMap` (if it is present) |
| Cyan squares (up to four, to the right of the first three slots) and nothing else | Loader step where it stopped: 1 memory map read, 2 tables reserved, 3 physical map built, 4 kernel mapped |
| AriBoot traces up to `ExitBootServices -> AriKernel`, no squares | The framebuffer announced by the `GOP:` line is not the screen (wrong address or format); note down that line |
| White square | In `ExitBootServices` (firmware) |
| White and green | In the jump to the kernel: page tables, stack or `BootInfo` validation |
| White, green and orange, no text | Between kernel entry and its console (initrd hash), or the console does not draw |
| Kernel text | Follow the traces; see the previous table |

## Storage

The first boot does not need to write anything. When the block service on
the USB stick is tested (AHCI/NVMe/USB have no driver yet; the USB stick is
only read by the firmware to boot), it will be done first in read-only mode
and always on the USB stick itself or a dedicated USB drive.

## Power-off and line editing (2026-10-11)

- `shutdown` powers the machine off through ACPI S5: at boot the kernel
  reads the FADT and the `\_S5_` package of the DSDT and logs
  `acpi: power-off through S5 (SLP_TYP a/b, PM1a ..., PM1b ...)`. If the
  firmware ignores the request, the console says
  `ACPI power-off had no effect; halting (it is safe to switch off)`.
- The shell edits the line like a terminal: Left/Right, Home/End
  (Ctrl-A/Ctrl-E), insert and delete anywhere (Backspace, Delete), Ctrl-U to
  clear, Up/Down for the history.
- HTTPS works with `http https://...` (TLS 1.3, ADR 0026): the server
  certificate is checked against embedded roots, the host name and the
  date from the real-time clock (`rtc:` line at boot). A private CA can be
  trusted by saving its certificate (DER) as `trusted_ca.der` in AriFS.

## SSH into the laptop (ADR 0025)

Build the image with your public key (only `ssh-ed25519` keys) and write it
as usual:

```bash
cargo xtask usb-image --cmdline "shell kbd=es" --authorized-key ~/.ssh/id_ed25519.pub
```

With the network cable connected, the boot log shows a `DHCP` line with the
address (if another host already uses the offered address, the log says so
and the laptop asks for another one), then
`sshd: listening on port 22, host key SHA256:..., 1 authorized key(s)`;
`net` shows the address. From the other machine:

```bash
ssh ari@192.168.1.24            # interactive shell (any user name)
ssh ari@192.168.1.24 diag       # a single command
```

The first connection asks to confirm the host key: compare it with the
`SHA256:` fingerprint printed by `usb-image` (the key is generated on this
machine and kept in `build/ssh_host_ed25519_key`) and shown on the laptop's
screen. One connection at a time; output of programs started with `run`
still appears on the laptop's screen.

Without `--authorized-key` the image has no `authorized_keys` and sshd does
not listen: no port is open. Refused keys are logged on the laptop with the
peer's address.

## Updating without the USB stick (ADR 0027, ADR 0029)

On the development machine, from the repository:

```bash
cargo xtask serve-update --version 2 --port 8765 [--app examples/hello_ext:2]
```

It builds and signs the current system image (and the given app packages),
writes the signed `index` and `index.sig` into `build/update/` and serves it
on the LAN (any free port; the default 8080 is often taken). Bump
`--version` on every publication: the laptop never reinstalls the version it
runs and refuses an older one.

**First time from a laptop running 0.9.0 (transition release).** Its
updater still speaks ADR 0027 and only reads `manifest.txt`, which
`publish-update` keeps writing next to the image for this release. On the
laptop, once:

```text
update http://192.168.1.16:8765/     # the old command: fetches this build through manifest.txt
reboot
```

After that reboot the laptop runs the new updater, which turns its saved
`update_url` into the first source. Once every machine is past this step,
publish with `--no-legacy-manifest`.

**From then on**, console or SSH:

```text
update add 192.168.1.16:8765         # only if no source is registered yet
update                               # verify the index, show what is new (installs nothing)
upgrade                              # verify and install it; the system goes to the inactive slot
reboot
```

The loader boots the new slot; if it does not come up, three attempts later
it returns to the previous one. `update sources` lists the sources, `update
remove N` drops one, `cat updater/history.log` shows what was installed and
`upgrade --allow-downgrade` is the only way back to an older version. The
source can also be baked into the stick with `cargo xtask usb-image
--update-source URL`.

**Signing key.** Without `--signing-key DIR` everything is signed with the
development key, whose seed is in the repository: anyone on the network
could sign an update the laptop accepts, and xtask says so on every
publication. For a laptop on an untrusted network, create a key outside the
repository (`cargo xtask keygen /secure/dir`), write the stick with
`cargo xtask usb-image --signing-key /secure/dir ...` and publish with the
same `--signing-key` (ADR 0029, "The signing key"). A laptop installed with
the development key does not accept the production key until it is
reinstalled from such a stick.
The project's release key (public half in `keys/release/`, fingerprint in
its README) signs everything published since 2026-10-17; the laptop was
reinstalled from a stick built with it the same day.
