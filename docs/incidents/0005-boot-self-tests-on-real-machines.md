# Incident 0005: boot self-tests write to and probe the real machine

- **Opened:** 2026-10-17. **Status:** open.
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
