# ADR 0032: Boot self-tests only on request (`selftest`)

- **Date:** 2026-10-17
- **Status:** Accepted (incident 0005; amends ADR 0007)

## Context

init grew its boot sequence out of the phase tests: every boot ran
`fs_client` (creates and rewrites `nuevo.txt` in AriFS), `blk_client`
(writes a pattern sector into the raw area at the tail of ARI0),
`net_client`, `tcp_client` and `http_client` (UDP, DNS, TCP and HTTP
against the QEMU test hosts at 10.0.2.2) and `input_client` (decodes the
key and mouse events QEMU injects). On the laptop that meant test data
written to the owner's stick on every boot, one of the 32 AriFS entries
taken by a test file, and, with virtio-net, packets to whatever answers at
10.0.2.2 on the owner's LAN. The `update.fs_nearly_full` scenario exposed
it (incident 0005).

## Decision

1. **Two kinds of boot checks.** init keeps on every boot the checks that
   touch nothing outside the machine and cost little: `hello`,
   `kernel_read` killed by a fault, the SPAWNER rights, a missing initrd
   name, `crashy_driver` restarted, the PCI count; and read-only health
   checks of the services it depends on: the block service answers Info and
   a read of the GPT (`block service answers: N sectors, GPT read, ARI0 at
   LBA L`), the file service answers a list (`file service answers: N
   files`), the network service reports its address within 3 s (`network
   service has address A`, or `no address yet (DHCP pending)`, which is not
   a failure), and every input service answered Info in its start probe
   (`N input device(s) ready`). A failed block or file health check stops
   the boot with the codes the test clients used (11, 14).
2. **Test-only checks behind `selftest`.** `fs_client`, `blk_client`,
   `net_client`, `tcp_client`, `http_client` and `input_client` run only
   when the command line carries `selftest`
   (`aricode_cmdline::keys::SELFTEST`, read by init through
   `sysinfo::CMDLINE` like `strict`). On a Realtek network they never run,
   with or without it (ADR 0024). The older test keys (`restart_storm`,
   `service_hang`, `init_fail_late`) stay as they were.
3. **QEMU scenarios get `selftest` by default.** xtask appends it to the
   cmdline of every boot (`Scenario::boot_cmdline`), so no scenario stops
   testing silently; a scenario opts out with `selftest = false`. Opting
   out: `init.no_selftest` (boots like a real machine and asserts that no
   test client line appears, `ls` shows the 4 files of a fresh disk and
   the runner's test hosts saw `0 contact(s)`), `usb.image_boot` (the stick
   image as written) and the two nearly-full-AriFS update scenarios, whose
   free entries belong to the updater. A unit test keeps the
   `fs-nearly-full`/`fs-one-short` disks off `selftest`.
4. **The runner counts contacts with the test hosts.** The host responder
   (UDP echo/DNS, TCP echo, HTTP) counts every datagram and connection and
   the capture ends with `[host] test hosts: N contact(s)`.
5. **The stick image carries no test data.** `cargo xtask usb-image` no
   longer writes the `ARIDISK` raw-area pattern (only `blk_client` uses
   it); ARI0 holds only the sample files. `usb-image` never adds
   `selftest`; a `selftest` boot from the stick stops at `blk_client`'s
   pattern check, before its write.

## Consequences

- A normal boot (`shell kbd=es` on the laptop) writes nothing to the stick
  except what the system itself needs (A/B slot confirmation, files the
  owner or a service such as the updater writes), and contacts no test
  host. The `input_client` idle rounds (about 1.6 s) are gone from it, and
  keys typed early are no longer swallowed by a test program.
- Boot health is still visible in the log and still stops the boot when the
  storage stack does not answer.
- Every new boot-time test program goes behind `selftest`; a check may run
  on every boot only if it is read-only and touches nothing outside the
  machine.
- The QEMU log of a `selftest` boot shows `AriInit: selftest: running the
  test-only boot checks`, so a real machine booted with it by mistake says
  so on screen.
