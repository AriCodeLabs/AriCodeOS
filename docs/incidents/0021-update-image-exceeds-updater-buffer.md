# Incident 0021: a system image larger than the updater's buffer reads as "installed version unknown"

- Opened: 2026-10-23. Status: fixed on `fix/update-image-size`
  (2026-10-24) for images on the disk; downloads keep a memory limit, now
  6 MiB, by ADR 0029's rule (see Fix and Open).
- Found by: the dev-build runs of incident 0018 (`cargo xtask test --debug
  --all --area network` on `fix/net-stack-headroom`), confirmed by the
  independent audit of that branch.
- Environment: any build whose system image (header + signed container
  with kernel and initrd) is over 4 MiB. Today that is every dev build;
  the release image is 70% of the limit.

## What was seen

In the dev-build run of incident 0018, 13 scenarios failed, every one of
them an update scenario (`update.*`, `apps.downgrade_*`,
`apps.upgrade_network`), none of them with a stack overflow. In
`update.migrate_legacy_url`:

```text
updater: source 1 http://10.0.2.2:36653/update/: index verified (serial 1792713578, system 2, 0 app(s))
updater: system: installed unknown, source 1 offers 2: installed version unknown; upgrade refuses it without --allow-downgrade
updater: everything is up to date
```

The scenario expects `installed 1, source 1 offers 2: upgrade available`.

## Measurement

- The dev system image published by the scenario
  (`build/scenarios/update.migrate_legacy_url/update/aricodeos-2.img`) is
  4,369,040 bytes; the release image is 2,932,392 bytes.
- The updater loads and verifies the active slot's image in a DMA buffer
  of `IMAGE_BUFFER` = 4 MiB = 4,194,304 bytes (`apps/updater/src/main.rs`,
  line 56). `Disk::load` returns `None` when the image's sectors do not fit
  that buffer, and `active_version` turns `None` into "unknown".

## Mechanism

An installed image that is too large for the buffer is indistinguishable,
for the updater, from one that cannot be read or does not verify: both are
"installed version unknown", and by ADR 0029 decision 5 an unknown
installed version makes `upgrade` refuse any offer without
`--allow-downgrade`. So the message is misleading (the image is intact,
only too large to load), and the advice it implies (force a downgrade) is
wrong. How an offered image over the limit is treated was not examined.

## Consequences

- Dev builds cannot run the update scenarios at all.
- A release image that grows by about 1.2 MiB more hits the same wall on
  real machines: every machine would report its installed version as
  unknown and refuse upgrades unless forced.

## Reproduction (2026-10-24)

`cargo xtask test --debug --all --area update` on develop 50e8ce6: 25 of
37 scenarios failed, every one through the same path (the debug image is
now 4,546,680 bytes; the release image 3,052,168).

## Fix

- One verification path that reads a container piece by piece:
  `aricode_initrd::locate` walks the archive through a reader with exactly
  the checks of `Initrd::parse` (a property test compares both on 6000
  damaged archives, read in pieces of 1, 3, 64 and 4096 bytes), and
  `Image::verify_streaming` verifies the manifest's signature and hashes
  kernel and initrd in 4 KiB pieces. `Image::verify_with`, which AriBoot
  uses, is now that function over a slice, so loader and updater share
  the code.
- The updater verifies images on the disk (active slot, rollback target,
  `UPDATE`) through the block client's buffer: their size is bounded by
  the partition, not by memory. `update apply` copies `UPDATE` into the
  inactive slot sector by sector and verifies the copy in the slot before
  writing its header, so what boots is what was verified.
- An installed version that cannot be read is never unexplained: the
  reason (empty slot, header larger than the partition, unreadable, or
  which check failed) is said and recorded (`upd-active-image`, code 59).
- Downloads stay in memory (ADR 0029, decision 3). The buffer is
  allocated only when `upgrade` downloads (the DMA quota is cumulative),
  and is now 6 MiB (`IMAGE_DOWNLOAD_MAX`, shared with xtask) within the
  updater's 8 MiB, the manifest maximum. An offered image over it is
  refused naming both sizes; `publish-update` refuses to publish one and
  warns past three quarters.
- Tests: the reproduction above passes; `update.large_image` boots, applies
  and rolls back 7 MiB images (disk `large-a`, padded past the download
  buffer); host tests for the walker, the streaming verifier (failing
  reader, oversized manifest) and the publish guard.

## Open

- Downloads larger than 6 MiB need the owner's decision, since each way
  changes a rule: stage the download in `UPDATE` and install it with the
  copy-and-verify path above (ADR 0029 chose memory so that nothing is
  written before verification), write it straight into the inactive slot
  (a failed download would cost the rollback image), or raise the
  manifest memory maximum for system apps. Today the release image is
  48% of the limit.
