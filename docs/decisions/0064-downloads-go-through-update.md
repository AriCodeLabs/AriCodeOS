# ADR 0064: System image downloads go through `UPDATE`

- **Date:** 2026-10-30
- **Status:** Accepted. The owner left the choice to this ADR's author on
  2026-10-29, to be decided by what has historically gone wrong with
  updates (ADR 0063). Changes ADR 0029, decision 3, for system images,
  and closes incident 0021's open item.

## Context

Until now `upgrade` downloaded a system image into a 6 MiB buffer in the
updater's memory and verified it there before writing anything (ADR 0029,
decision 3). Only then did it copy the image into the inactive slot. Two
things made this a limit:

- No image larger than the buffer could be installed over the network,
  while slots and `UPDATE` hold 12 MiB (incident 0021).
- The updater kept 8 MiB of memory for a buffer it used once per run.

Images will grow as code is adopted under ADR 0063.

There are two ways the established systems update, and what has gone
wrong with each:

- **Writing straight into the inactive slot as the download arrives**
  (Android A/B streaming, ChromeOS). It needs no extra space and writes
  once. But a failed or forged download destroys whatever the inactive
  slot held. On AriCodeOS that slot holds the system before the last
  install, which is `update rollback`'s way back (ADR 0045). A flaky
  network during an update would cost the owner that way back.
- **Downloading into a staging area, verifying, then applying** (Windows
  Update, macOS, `apt`). Updates that can be applied only when complete
  and verified are the answer to the most common complaint about
  updates: a machine left unbootable, or without its previous system, by
  an interrupted or bad update. The usual cost is free space, and here
  `UPDATE` already exists with exactly that role for `update apply`.

## Decision

1. **`upgrade` downloads a system image into `UPDATE` as it arrives.**
   - `UPDATE`'s header sector is zeroed and flushed when the first byte
     arrives, so a cut download never leaves a valid image there, and a
     run that receives nothing leaves `UPDATE` as it was.
   - The container's sectors are gathered in the block client's buffer
     and written 16 sectors at a time.
   - The image's own header is held back in memory.
   - The size limit is the `UPDATE` partition, measured on the machine.
     Memory plays no part.

2. **What the disk holds is what is checked.**
   - The image's SHA-256 is computed by reading `UPDATE` back, not over
     the bytes received, and is compared with the signed index.
   - Then the image is verified from the disk: its signature and hashes,
     the channel's key, and the version and commit the index names.
   - Only then is its header written. `UPDATE` never holds an image
     `upgrade` refused, which `update apply` could otherwise install.

3. **It installs as `update apply` does.** The verified image is copied
   from `UPDATE` into the inactive slot:
   - the slot's old header is zeroed first;
   - the bytes copied are hashed on the way and must match the index
     again;
   - the copy is verified in the slot;
   - the downloaded header is written last;
   - the slot is then scheduled with three tries.

   So the slot holds exactly the bytes the index names, unsigned
   container entries included.

   This is the path incident 0021 built and `update.large_image` already
   covers. The running system and the inactive slot's previous system are
   untouched until the image has passed every check.

4. **`UPDATE` is the updater's staging area.** An `upgrade` that
   downloads a system image replaces whatever `UPDATE` held. After a
   successful upgrade it holds the installed image, and `update apply`
   would install that same image.

5. **The updater's memory goes from 8 MiB to 1 MiB.** Its download buffer
   and the code that installed from memory are removed.

6. **Publishing.** `SYSTEM_IMAGE_MAX` replaces `IMAGE_DOWNLOAD_MAX`. It is
   12 MiB: what the slot and `UPDATE` partitions of every layout hold, and
   a host test checks each layout against it. `publish-update` refuses a
   larger image and warns past three quarters.

## Consequences

- An image up to 12 MiB installs over the network. A larger one needs
  larger partitions, which is a new layout, so a reinstall.
- A download that fails, is cut, or does not match the index costs
  nothing but `UPDATE`'s previous contents.
- The extra work is a local copy and a few reads of `UPDATE`. It made no
  measurable difference:

  | Scenario | Before (s) | After (s) |
  |---|---|---|
  | `update.network` | 19.0 | 18.1 |
  | `update.redirected_source` | 9.7 | 10.1 |
  | `update.dev_channel` | 30.0 | 30.5 |
  | `update.cut_during_install` | 26.0 | 26.0 |

  The 7 MiB image of `update.large_download` downloads, verifies,
  installs and boots in 21 s.
- New code: `UPDATE_IO` (16, `upd-update-io`) when `UPDATE` cannot be
  written or read back during a download. `NO_MEMORY` (11) is gone with
  the buffer.

## Audit (2026-10-30)

An independent audit found no high or medium findings. It found no path
that schedules an unverified image, writes the active slot, or touches the
inactive slot before every check has passed. Its low findings and what
was done:

- **L1.** The header was written before the signature and version checks,
  so an image `upgrade` refused stayed in `UPDATE` for `update apply` to
  install.
  - Fixed: the image is verified first and sealed after.
  - `update.dev_stable_image` now runs `update apply` after the refusal.
    It failed before the fix (the refused image was installed into
    slot B) and passes after it.
- **L2.** A read error while verifying `UPDATE` was reported as a bad
  signature. It is now `upd-update-io`.
- **L3.** `UPDATE` was cleared before any network contact. It is now
  cleared on the first byte received.
  - What remains: the HTTP client also hands the body of an error answer
    (a 404 page) to the download, so such an answer still replaces
    `UPDATE`'s contents. That fits decision 4.
- **L4.** Nothing stops two updater runs at once (two shell sessions). One
  run's download can overwrite `UPDATE` while another copies it.
  - The copy's hash and its check refuse to schedule the mix.
  - But the inactive slot's header was already zeroed, so its previous
    system is lost.
  - Open: a single-instance rule for the updater. The old flow raced on
    slot writes in the same way, over a shorter window.
- **L5.** The large-image test could not see lost writes in 7 MiB of
  zeros. Now:
  - the padding is a non-zero pattern;
  - its length leaves the last sector partial;
  - the scenario starts with another image in `UPDATE`.
- **L6.** The slot was bound to the index only through the signature. The
  copy is now hashed against the index, and the downloaded header is the
  one written.
- **L7.** An 8 KiB heap buffer stayed alive through the TLS handshakes.
  Bytes are now gathered in the block client's DMA buffer.
- **L8.** Stale text about the memory buffer was fixed in the updater, in
  ADR 0029 and in `update.large_image`.

## Tests

- `update.large_download`: an image of 7 MiB of padding plus the system,
  larger than the updater's whole memory, is downloaded, verified,
  installed and booted from slot B.
- `update.failed_download_keeps_rollback`:
  - B (version 2) boots and is confirmed;
  - an altered version 3 is downloaded and refused by its SHA-256;
  - `update rollback` still finds A's version 1 and schedules it.
- `update.tampered_image` and `update.withdrawn_image` keep passing:
  nothing is installed.
- Host tests:
  - every layout holds `SYSTEM_IMAGE_MAX`;
  - the large-image padding exceeds the updater's manifest memory;
  - the publish guard refuses an image over the limit.
- `cargo xtask test --all --area update`: 45 of 45.
