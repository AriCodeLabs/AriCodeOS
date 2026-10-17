# Incident 0005: boot self-tests write to and probe the real machine

- **Opened:** 2026-10-17. **Status:** fixed and audited on `fix/boot-self-tests`, pending
  the QEMU run of the listed scenarios.
- **Found by:** the QEMU run of `update.fs_nearly_full` (feature/update-sources):
  a file `nuevo.txt` the scenario did not expect took the last free AriFS
  entry.

## Facts

init runs its self-test programs on every boot, also on real machines
booted with `shell`:

- `fs_client` (`services/init/src/main.rs:348`,
  `user/testprogs/src/bin/fs_client.rs:55-96`) creates `nuevo.txt` in the
  AriFS root if it does not exist, replaces its content and reads it back:
  every installed system carries a test file that takes one of the 32
  table entries.
- `blk_client` (`services/init/src/main.rs:361`,
  `user/testprogs/src/bin/blk_client.rs`) reads the raw area at the tail of
  ARI0 and **writes a pattern** at `raw + 100` on the system disk, the USB
  stick on the laptop, on every boot.
- `net_client`, `tcp_client` and `http_client` (init ~1800-1840) contact
  the QEMU test hosts at 10.0.2.2 (UDP responder, TCP echo, HTTP server):
  on a real network they send packets to whatever answers at that address
  and wait for their timeouts.
- `hello`, `kernel_read`, `crashy_driver`, and the other supervisor
  self-tests run too; they touch no disk or network.

These were boot-time checks of the QEMU test environment that grew into
every boot. Writing test data to the owner's disk, consuming AriFS table
entries and sending packets to an arbitrary host on the owner's LAN are
not acceptable on a real machine, and the writes are exactly the kind of
early write that incident 0001 showed some sticks handle slowly.

## Plan

1. Decide which checks are boot health checks worth keeping on real
   machines (read-only ones: the block service answers, the file service
   lists, the network service has an address) and which are test-only.
2. Run the test-only ones only when the command line says so (a `selftest`
   key, set by the QEMU scenarios), never by default; keep the read-only
   health checks.
3. Remove `nuevo.txt` from the stick image's expectations; scenarios that
   count files adapt.
4. Review with an independent audit; record the change in the relevant ADR.

## Classification of the boot-time checks (2026-10-17)

(a) = read-only or self-contained, kept on every boot; (b) = test-only, run
only with `selftest` on the command line.

| Check (init `main`) | Class | Reason |
|---|---|---|
| `hello` exits with 42 | (a) | Spawns and reaps an initrd program; touches nothing outside the process table. Milliseconds. |
| `kernel_read` killed by a fault | (a) | Checks that the kernel isolates a faulting process; no I/O. Prints a `USER FAULT` line, expected. |
| SPAWNER without `MANAGE` is denied; unknown initrd name is `NotFound` | (a) | Capability checks on init's own handles; no process is created. |
| `crashy_driver` crashes, then restarts | (a) | Process fault and restart, no device, no disk. |
| PCI function count | (a) | Read of the kernel's PCI table. |
| Block service: Info and the GPT read (`find_partition` for ARI0) | (a) | Read-only; init needs the GPT anyway to find ARI0. New line `block service answers: N sectors, GPT read, ARI0 at LBA L`; failure stops the boot (Info 11, no ARI0 12; fsd not started 13). |
| File service: list of the AriFS root | (a) | Read-only; replaces `fs_client` as the health check. New line `file service answers: N files`; failure stops the boot (14). |
| Network service: address reported by Info | (a) | Read-only request to netd, no DMA buffer; DHCP is the system's own traffic, not test traffic. Runs after the USB devices; each request bounded to 250 + 250 ms; polls up to 2 s on virtio, a single request on Realtek. Not a failure (`no address yet (DHCP pending)`). |
| Input services: Info in the supervisor start probe | (a) | Already part of publishing the service; init now prints `N input device(s) ready`. |
| `fs_client` | (b) | Creates `nuevo.txt` (one of the 32 AriFS entries), writes, replaces and flushes it on every boot. |
| `blk_client` | (b) | Writes a pattern sector at raw area + 100 on the system disk and needs the `ARIDISK` test pattern. |
| `net_client` | (b) | UDP echo and DNS against the responder at 10.0.2.2:17007, ping to a never-answering 10.0.2.99. |
| `tcp_client` | (b) | TCP echo against 10.0.2.2:17008, a refused port, and a listener that waits 1.5 s for the host probe. |
| `http_client` | (b) | HTTP GETs against the test server at 10.0.2.2:17010. |
| `input_client` | (b) | Exists to decode the keys and mouse events QEMU injects over QMP; on a real machine it waits ~1.6 s and swallows what the owner types meanwhile. |
| `restart_storm`, `service_hang`, `init_fail_late` | (b), already gated | Each behind its own test key since it was added; unchanged. |
| A/B slot confirmation | not a check | The system's own state write (`confirm_boot_slot`, Phase 30, ADR 0027), only when a freshly installed slot is pending; stays. |

## Fix

- `aricode_cmdline::keys::SELFTEST`, a bare key (`has_flag`); init reads
  it through `sysinfo::CMDLINE` like `strict` and runs the (b) clients
  only with it. The health checks of (a) run on every boot, also before
  the clients when `selftest` is set.
- xtask appends `selftest` to the cmdline of every scenario boot unless the
  scenario says `selftest = false` (`Scenario::boot_cmdline`). Opting out:
  `init.no_selftest` and `init.no_selftest_usb` (new), `usb.image_boot`,
  `update.fs_nearly_full`, `update.fs_one_short`. The runner captures the
  guest network with `filter-dump` and appends `[host] guest frames
  other than ARP and DHCP: N`; the no-selftest scenarios require 0, `init.boot`
  forbids 0.
- `fill_arifs` no longer reserves an entry for `nuevo.txt`
  (`BOOT_CREATED_FILES` removed); a unit test keeps the nearly-full disks off
  `selftest`.
- `cargo xtask usb-image` writes only an English `README` into ARI0 (plus
  what `--authorized-key`/`--update-source` add), no `ARIDISK` raw-area
  pattern, and refuses test-only cmdline keys.
- Scenarios adapted: `update.slot_a` and `update.pending_boot` (the loader's
  `cmdline:` line now shows `selftest`), `filesystem.corrupt_detected` (the
  file health check fails before `fs_client` would run).
- ADR 0032 records the decision; notes in ADR 0007, 0024, 0029,
  `docs/architecture.md`, `docs/hardware.md` and `tests/README.md`.

## Open

- QEMU was not run on the branch (the test ports were in use by another
  process): `cargo xtask check` and the host tests pass; the scenarios
  listed in the branch report must run before merging.
- The keys `input_restart` and `shell_fault` named in the audit do not
  exist in the tree; `TEST_ONLY` lists the test keys that do.

## Audit (2026-10-17)

Independent audit of `fix/boot-self-tests`: safe to merge once QEMU
passes. Every finding is resolved on the same branch:

1. **Other boot-time writes were not documented; sshd wrote its seed on
   every boot.** sshd now saves `random_seed` on the first accepted
   connection, before drawing its randomness
   (`services/sshd/src/main.rs:387`, `:419-422`, doc at `:73-76`; ADR 0025
   updated). The host key creation (only without a key) and fsd's journal
   recovery at mount (`libs/arifs/src/lib.rs:678-698`, only after a cut)
   are documented in `docs/hardware.md:74-84` and ADR 0032
   (Consequences).
2. **The 3 s network wait.** The check now runs after the USB devices
   (`services/init/src/main.rs:2126-2130`), sends Info with `rpc::call`
   bounded to 250 ms send + 250 ms reply instead of `NetClient` and its
   5 s `GRACE_MS` (`:410-451`), polls up to 2 s on virtio
   (`:458`, `:1922`) and makes a single request on Realtek (`:1962`).
   Wording fixed in ADR 0032 (item 1), this file and
   `docs/hardware.md:62-66`.
3. **Contact counter.** Replaced by a capture of the guest network: QEMU
   `filter-dump` on `net0` (`tools/xtask/src/qemu.rs:772-778`), the guest
   MAC pinned (`:636`), and a count of the frames the guest sent (refined
   by re-audit L3 below). The responder
   no longer counts, so no thread state is read.
4. **No scenario on the laptop path.** `tests/qemu/init_no_selftest_usb.toml`
   (USB stick, USB HID only, `selftest = false`).
5. **`usb-image --cmdline` accepted test-only keys.**
   `aricode_cmdline::keys::TEST_ONLY` and `CmdLine::first_of`
   (`libs/cmdline/src/lib.rs:60`, `:134`, test at `:207`); refused in
   `tools/xtask/src/image.rs:360` (test at `:897`).
6. **`selftest=0` enabled selftest.** `CmdLine::has_flag`
   (`libs/cmdline/src/lib.rs:53`) in init
   (`services/init/src/main.rs:1809`) and xtask
   (`tools/xtask/src/scenario.rs:360`); test `selftest_is_a_bare_whole_key`.
7. **Health checks left clients allocated.** The file client of the health
   check is returned by `start_storage_stack` and reused by the restart
   storm and the broker (`services/init/src/main.rs:326`, `:394`, `:2152`,
   `:1282`, `:2188`); the network check uses no client and no DMA buffer.
8. **Stop codes in the docs.** 11 Info, 12 no ARI0, 13 fsd, 14 list:
   `docs/hardware.md:68-71`, ADR 0032 item 1, this file.

Owner's stick files (decision taken by the owner, reversible):
`write_usb_image` uses its own list `USB_FILES`, an English `README` with
the repository link (`tools/xtask/src/image.rs:318`, `:522`, test at
`:905`); no `hola.txt`, no Spanish README, no WASM demo. The test disk
keeps `DISK_FILES`. `usb.image_boot` expects 1 file.

### Re-audit (2026-10-17)

Safe to merge once QEMU passes; the four low findings are resolved:

- **L1** sshd logs a failed `random_seed` save and marks it saved only on
  success, so the next connection tries again
  (`services/sshd/src/main.rs:79-90`, `:430`).
- **L2** When sshd generates a host key on first start it saves the
  next-boot seed before `entropy.next()` draws the key
  (`services/sshd/src/main.rs:154`, `:164`, `:396`).
- **L3** The capture counts every frame the guest sent, of any ethertype
  (IPv4, IPv6, 802.1Q/802.1ad tags, others), except ARP and DHCP:
  `guest_frames` and `is_arp_or_dhcp` (`tools/xtask/src/qemu.rs:645`,
  `:679`), line `[host] guest frames other than ARP and DHCP: N` (`:932`);
  the host test covers VLAN-tagged ARP, DHCP and UDP, IPv6, another
  ethertype and a truncated IPv4 header (`:1031`). Scenarios and
  `tests/README.md` use the new line.
- **L4** Every key documented "Tests only" in `libs/cmdline` is in
  `TEST_ONLY` and vice versa, checked by
  `test_only_keys_match_their_documentation`
  (`libs/cmdline/src/lib.rs:175`; the docs of `qemu_exit`, `test`,
  `init_fail_late`, `restart_storm` and `service_hang` now say "Tests
  only"). `usb-image` also refuses the loader-owned `boot_slot` and
  `boot_pending` (`keys::LOADER_ONLY`, `libs/cmdline/src/lib.rs:145`;
  `tools/xtask/src/image.rs:365`, test at `:897`).
