# Incident 0005: boot self-tests write to and probe the real machine

- **Opened:** 2026-10-17. **Status:** fixed on `fix/boot-self-tests`, pending
  the QEMU run of the listed scenarios and the independent audit.
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
| Block service: Info and the GPT read (`find_partition` for ARI0) | (a) | Read-only; init needs the GPT anyway to find ARI0. New line `block service answers: N sectors, GPT read, ARI0 at LBA L`; failure stops the boot (11, 12). |
| File service: list of the AriFS root | (a) | Read-only; replaces `fs_client` as the health check. New line `file service answers: N files`; failure stops the boot (14). |
| Network service: address reported by Info | (a) | Read-only request to netd; DHCP is the system's own traffic, not test traffic. Waits at most 3 s and is not a failure (`no address yet (DHCP pending)`). Also on Realtek. |
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

- `aricode_cmdline::keys::SELFTEST`; init reads it through
  `sysinfo::CMDLINE` like `strict` and runs the (b) clients only with it.
  The health checks of (a) run on every boot, also before the clients
  when `selftest` is set.
- xtask appends `selftest` to the cmdline of every scenario boot unless the
  scenario says `selftest = false` (`Scenario::boot_cmdline`). Opting out:
  `init.no_selftest` (new), `usb.image_boot`, `update.fs_nearly_full`,
  `update.fs_one_short`. The runner's responder counts datagrams and
  connections to the test hosts and appends `[host] test hosts: N
  contact(s)` to the capture; `init.no_selftest` requires 0, `init.boot`
  forbids 0.
- `fill_arifs` no longer reserves an entry for `nuevo.txt`
  (`BOOT_CREATED_FILES` removed); a unit test keeps the nearly-full disks off
  `selftest`.
- `cargo xtask usb-image` no longer writes the `ARIDISK` raw-area pattern.
- Scenarios adapted: `update.slot_a` and `update.pending_boot` (the loader's
  `cmdline:` line now shows `selftest`), `filesystem.corrupt_detected` (the
  file health check fails before `fs_client` would run).
- ADR 0032 records the decision; notes in ADR 0007, 0024, 0029,
  `docs/architecture.md`, `docs/hardware.md` and `tests/README.md`.

## Open

- QEMU was not run on the branch (the test ports were in use by another
  process): `cargo xtask check` and the host tests pass; the scenarios
  listed in the branch report must run before merging.
- The stick still carries the sample files `hola.txt` and `README` (whose
  text says "disco de pruebas") and the WASM demo pair. They are sample
  content the shell and the demos use, not written by any check; whether
  an owner's stick should carry them is a separate decision.
- Independent audit of the change (failure protocol, step 6).
