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
   files`), every input service answered Info in its start probe (`N input
   device(s) ready`), and the network service reports its address
   (`network service has address A`, or `no address yet (DHCP pending)`,
   which is not a failure). The network check runs after the USB devices
   (step 9c), so it never delays the stick or the keyboard; it sends plain
   Info requests with no DMA buffer, each bounded to 250 ms for the send and
   250 ms for the reply (not `rpc::GRACE_MS`, 5 s). On virtio-net it polls
   for up to 2 s (QEMU's DHCP answers in milliseconds); on Realtek it makes
   a single request and does not wait. The file client of the health check
   is the one init keeps using (restart storm, broker): no client is
   allocated and abandoned. A failed storage check stops the boot before
   the shell: Info unanswered 11, no ARI0 12, fsd not started 13, list
   unanswered 14.
2. **Test-only checks behind `selftest`.** `fs_client`, `blk_client`,
   `net_client`, `tcp_client`, `http_client` and `input_client` run only
   when the command line carries the bare key `selftest`
   (`aricode_cmdline::keys::SELFTEST`, `CmdLine::has_flag`: `selftest=0`
   does not enable it), read by init through `sysinfo::CMDLINE` like
   `strict`. On a Realtek network they never run, with or without it (ADR
   0024). The older test keys (`restart_storm`, `service_hang`,
   `init_fail_late`) stay as they were.
3. **QEMU scenarios get `selftest` by default.** xtask appends it to the
   cmdline of every boot (`Scenario::boot_cmdline`), so no scenario stops
   testing silently; a scenario opts out with `selftest = false`. Opting
   out: `init.no_selftest` (virtio, boots like a real machine and asserts
   that no test client line appears, `ls` shows the 4 files of a fresh disk
   and the guest sent no frame besides ARP and DHCP), `init.no_selftest_usb`
   (the laptop path: USB stick and USB HID only, same assertions),
   `usb.image_boot` (the stick image as written) and the two
   nearly-full-AriFS update scenarios, whose free entries belong to the
   updater. A unit test keeps the `fs-nearly-full`/`fs-one-short` disks off
   `selftest`.
4. **The runner records the guest's network traffic.** Every scenario boot
   captures the guest network with QEMU `filter-dump` (`net.pcap` in the
   scenario directory, the guest MAC pinned to 52:54:00:12:34:56) and the
   capture ends with `[host] guest frames other than ARP and DHCP: N`:
   every frame the guest sent, of any ethertype (IPv4, IPv6, VLAN-tagged or
   other), except ARP and DHCP. This also catches traffic the host
   responder never sees (ICMP, IPv6, other hosts).
5. **The stick carries no test or sample data.** `cargo xtask usb-image`
   writes its own file list into ARI0: an English `README` (what the stick
   is and the repository link), plus what `--authorized-key` and
   `--update-source` install; no `hola.txt`, no test README, no WASM demo,
   no `ARIDISK` raw-area pattern. The test disk keeps its files. A
   `selftest` boot from such a stick stops at `fs_client`'s first check,
   before any write.
6. **Test-only keys are refused for a real machine.**
   `aricode_cmdline::keys::TEST_ONLY` lists every key documented "Tests
   only" (`qemu_exit`, `test`, `init_fail_late`, `restart_storm`,
   `service_hang`, `input_restart`, `shell_fault`, `selftest`,
   `usb_fault`; a unit test keeps the list and the documentation in
   step), and `keys::LOADER_ONLY` the keys AriBoot
   adds itself (`boot_slot`, `boot_pending`); `cargo xtask usb-image
   --cmdline` refuses a line with any of them.
7. **No early write by sshd.** sshd rewrites `random_seed` before it
   draws randomness for something that lasts: when it generates a host key
   on first start, and otherwise on the first accepted connection of a
   boot, before drawing that connection's keys. A failed save is logged
   and retried on the next connection. Not at start: a boot nobody
   connects to writes
   nothing.

## Consequences

- A normal boot (`shell kbd=es` on the laptop) writes no test data and
  contacts no test host. What the system itself may still write at boot:
  the A/B confirmation of a freshly installed slot, fsd's journal recovery
  at mount after a power cut (a pending replace redone, a broken journal
  cleared), and sshd's host key the first time it starts without one
  (`usb-image --authorized-key` installs it). The `input_client` idle
  rounds (about 1.6 s) are gone, and keys typed early are no longer
  swallowed by a test program.
- Boot health is still visible in the log and still stops the boot when the
  storage stack does not answer.
- Every new boot-time test program goes behind `selftest`, and every new
  test key into `TEST_ONLY`; a check may run on every boot only if it is
  read-only and touches nothing outside the machine.
- The QEMU log of a `selftest` boot shows `AriInit: selftest: running the
  test-only boot checks`, so a real machine booted with it by mistake says
  so on screen.
