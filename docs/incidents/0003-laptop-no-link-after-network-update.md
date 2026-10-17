# Incident 0003: no Ethernet link after booting a network-updated slot

- **Opened:** 2026-10-17. **Status:** open.
- **Hardware:** the reference laptop (Realtek 10ec:8136, `rtl_net`),
  same cable and router as minutes before.

## What was seen

1. Boot from the stick (slot A, `develop` aaa8c25, `strict`): network up,
   SSH working. `update` downloaded `aricodeos-2.img` from the channel on
   the development host, verified and installed it into slot B;
   `reboot`.
2. The laptop booted slot B, init confirmed it (`booted from slot B
   (pending confirmation)`, `boot confirmed on slot B`), sshd listened,
   but `net` printed `link down (cable?)` and `no address (DHCP
   pending)`. The laptop did not answer on the LAN (no ARP reply for
   192.168.1.24, its MAC not seen).
3. After a further restart of the laptop and unplugging/replugging the
   cable: `booted from slot B (active)`, still `link down`, no DHCP line.

## Facts

- The image in slot B was not a deliberate publication: a QEMU test run
  of 2026-10-12 07:02 (scenario `update.network`, `host_update`) had
  written its version 2 into `build/update/`, the directory the real
  channel server (`cargo xtask serve-update`) serves. The laptop received
  that test image.
- Slot A, minutes earlier, had link and DHCP with the same cable.

## Hypotheses

- H1 Something in the 2026-10-12 07:02 build makes `rtl_net` miss the
  link on this hardware.
- H2 The Realtek NIC is left in a state the driver does not reset (warm
  reboot, then a restart), independently of the image.
- H3 Cable or port.

## Actions

- Design fix (done): the scenarios publish and serve their channel from
  `build/update-test/` (`image::test_update_dir()`); `build/update/` is
  written only by `publish-update` / `serve-update`.
- Discriminate: reflash the stick (slot A active with the current
  build). Link up there → H1 (then compare the two builds of `rtl_net`
  and init); link down → H2/H3.
