---
name: Hardware test report
about: You booted AriCodeOS on a real machine from the USB stick image
title: "Hardware: <make and model>"
labels: hardware
---

<!-- Thank you for testing on real hardware: it is the part no QEMU run can cover.
     Follow docs/hardware.md (UEFI boot, Secure Boot off, a stick you can wipe; the
     internal disk is never touched). The internal keyboard and a USB keyboard both work. -->

## Machine

- Make and model:
- CPU and chipset (if known):
- Firmware: UEFI version, Secure Boot off?
- Network: Ethernet controller (`devices` lists the PCI ids)
- USB controllers (`devices`: xHCI 0c.03.30, EHCI 0c.03.20) and the stick used

## Image

- Version or commit, and the command line used (`cargo xtask usb-image ...`):

## What happened

<!-- Did it boot to `ari>`? Which of these work: keyboard, `net` (link, DHCP address),
     `ping`, `dns`, `http`, `ls`/`cat` (file service), `update`/`upgrade`, `reboot`,
     `shutdown`? If it stopped earlier, what was the last line on screen? -->

## Evidence

<!-- `diag` output and a photo of the screen if something failed. `diag diag.txt`
     writes the report to the stick; back on your PC, `cargo xtask fs-cat /dev/sdX diag.txt`
     reads it (docs/hardware.md). If it did not reach the shell, a photo of the last
     screen is enough to start. -->
