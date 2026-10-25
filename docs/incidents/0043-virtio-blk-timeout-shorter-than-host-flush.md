# Incident 0043: virtio-blk gave up on requests a busy host disk took over 2 s to complete

- Opened: 2026-10-25. Status: fixed on `fix/blk-request-timeout`.
- Found by: `cargo xtask test --all --area storage,filesystem,boot,update,init,usb`
  on `fix/storage-bounds` (`e4b2357`): `filesystem.descriptors_reclaimed`
  and `crypto.carried_seed` failed at the same moment; a run of those areas
  alone passed. The same signature was in an earlier CI report nobody had
  investigated (`aricodeos-ci-reports/2026-10-25T0053Z-develop-0fd600a`,
  `--cpus 4`: five scenarios with `random seed not saved`, two hung inside
  AriBoot's own polled disk I/O).
- Environment: QEMU, ten guests at once on a host whose root filesystem
  is 90 % full on a consumer NVMe, with another service doing fsyncs. The
  same 2 s limit applied on real hardware.

## Evidence

- `descriptors_reclaimed`: the first read of the boot (sector 1) did not
  complete: `virtio-blk: no GPT`, `resetting the device: a request did not
  complete`; later a request of `fs_client` did the same and the boot
  failed. `carried_seed`: init's first write and flush to ARIBOOT failed
  (`random seed not saved`); a later one worked.
- Host side, at 15:32:09-15:32:30: load 4.1 to 8.8 with no thermal pause,
  write bursts of 164-227 MB/s, no scenario starting or ending for 16 s,
  `gui.two_windows` taking 33 s instead of 11 s with an identical guest
  log. In the CI occurrence, timed-out QEMU processes sat in D state
  (blocked on host I/O) after SIGKILL, and AriBoot (firmware polling, no
  interrupts at all) stalled the same way.

## Mechanism

QEMU runs the scenario disk with its defaults (writeback cache, I/O in
threads), so a guest flush is an `fdatasync` of the image on the host.
Each scenario wrote a fresh 48 MiB image, almost all zeros, so every boot
put 48 MiB of dirty pages on the host and its first flush waited for them
to reach the SSD, behind the other guests and the other service's
syncs. The driver gave a request 2 s, then reset the device and failed
the request. An independent study traced the interrupt path (pending
count latch, APIC EOI, MSI-X programming, virtqueue fences) and found no
way for a completion to lose its interrupt; it could not be ruled out
from the lost logs, so the driver now says so if it ever happens.

## Fix

- `REQUEST_TIMEOUT_MS` is 30 s on every platform, the window Linux gives
  SCSI and NVMe commands; the client's 90 s reply wait covers it.
- On a timeout the driver checks the used ring once more: a completion
  there is taken and reported (`a request completed without its
  interrupt`, event `blk irq missing`), so a lost interrupt can never pass
  for a slow device; otherwise it says whether the device asked to be
  reset.
- `cargo xtask` writes scenario disks sparsely (all-zero 64 KiB chunks
  left as holes), so a boot no longer puts 48 MiB of dirty pages on the
  host.
- `storage.late_completion`, which waits out an injected lost doorbell
  twice, gets 180 s.

## Audit (2026-10-25)

- The test programs' own reply waits were sized for the 2 s limit:
  `blk_late` (15 s, `storage.late_completion` failed) and `blk_client`
  (5 s, every selftest boot) now use `block::REPLY_WAIT_MS` (90 s).
- With `strict`, a completion found without its interrupt halts the
  driver (code 46) instead of being taken.
- fsd stamps a chunked replace's staging time after the chunk is written,
  not before: slow block requests no longer make a live transaction look
  abandoned to the next writer.
- `block::REPLY_WAIT_MS`'s documented chain now includes virtio-blk
  (32 s per timed-out request, two clients).
- Accepted: the wait is a fresh 30 s per wake-up, not one deadline per
  request; the queue's vector only fires on completions, so a wake-up with
  nothing to pop happens at most once (a late interrupt latched from the
  previous request). Callers that wait 5 s on init's broker or fsd
  (`launch::spawn`, `service_status`, sshd's session request) can time out
  while a slow but healthy disk request runs; the launch then still
  happens. The USB image is still written whole (it is one per run, not
  one per scenario). A head id out of range in the used ring is handled on
  `fix/driver-bounds` (incident 0033).
