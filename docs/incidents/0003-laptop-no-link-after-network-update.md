# Incident 0003: no Ethernet link after booting a network-updated slot

- **Opened:** 2026-10-17. **Status:** H1 supported by a deliberate
  reproduction (below); a separate link drop while running is being
  watched.
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

## Result of the discrimination (2026-10-17)

Stick reflashed with `develop` a236c68 (slot A active), same cable and
router port: `booted from slot A (active)`, `link up`, DHCP address
192.168.1.27 (the router no longer hands out .24), SSH working. The only
fault event is the known `SYNCHRONIZE CACHE` rejection at boot (sense
5/20/00).

So the NIC, the cable and the port work with the current build, and the
link was missing only with the test-built image of 2026-10-12 07:02 in
slot B, across a warm and a further restart. H1 is the leading
hypothesis; H2 (a NIC state left by the warm reboot that only that boot
sequence hit) is not excluded, because the stick reflash also meant a
full power cycle. Next: reproduce on purpose by publishing an image from
the current build to the real channel, updating the laptop and
rebooting into slot B. If the link comes up, the 2026-10-12 build is the
suspect and its `rtl_net`/init are compared with the current ones; if it
does not, the warm reboot path of `rtl_net` is investigated.

## Deliberate reproduction (2026-10-17)

Version 3 (develop 45a69ef) published from the development host to the
real channel, downloaded by the laptop with `update` and `upgrade`
(ADR 0029 sources, index verified) into slot A, then `reboot`: `booted
from slot A (active)`, `link up`, DHCP 192.168.1.27, SSH working, both
EHCI controllers ready. The same path again with version 4 (develop
df602d1) into slot B: link up, SSH working. A network-updated slot boots
with a working link when the image comes from the current build, so the
link-less boots were tied to the test-built image of 2026-10-12 (H1) and
not to the network update path or the warm reboot. What that image had
that the current one has not remains unknown: the image was removed with
the stale channel, so it cannot be compared any more.

## Separate observation: link drop while running (2026-10-17)

With version 3 running for about two hours (uptime 7257 s at the next
command), the laptop stopped answering on the network. On screen, `net`
showed `link down (cable?)` with the address still assigned. Reseating
the cable at both ends brought the link up at once and SSH worked again
with no reboot. No fault event was recorded. Whether the cable or the
PHY link handling of `rtl_net` dropped the link is undecided: next time
it happens, run `diag` before touching the cable and note whether the
router port LED is lit.
