# ADR 0072: AriFS v2, copy-on-write replace and a larger partition

- **Date:** 2026-11-03
- **Status:** Proposed. The decision is the owner's: it changes the
  on-disk format, the disk layout and what an older system can read
  (incident 0026, "Open"; owner decisions pending, item 2).

## Context

An installed app is a signed package at `apps/NAME.aripkg` (ADR 0017),
installed by one atomic `replace` (ADR 0018). AriFS v1 (ADR 0014) bounds
that, and everything else it stores:

| Limit | AriFS v1 |
|---|---|
| Files | 32, in one flat table |
| File capacity | 64 sectors (32 KiB), fixed at creation |
| Atomic replace | 32 KiB: the data is journalled first, in a journal of one file's capacity |
| Delete, rename | none |
| Partition `ARI0` | 3 MiB (6110 sectors on the test disk, 6144 on the stick) |

What would have to fit, measured on 0.17.0 (stripped release binaries):

| Program | Bytes |
|---|---|
| `updater` (HTTPS) | 1,069,680 |
| `shell` | 1,036,880 |
| `wasm_runner` | 901,704 |
| `sshd` | 275,024 |
| `hello_ext` (the external example) | 14,280 |

Any app that uses TLS or WebAssembly is near 1 MiB. Neither the format nor
the partition can hold one, and an atomic replace needs room for the old
and the new copy at once. Today's use is small: the reference laptop holds
12 files, the largest 4 KiB (`bench/medidas`), and no app.

## What established designs do, and what went wrong

Each design's mechanism and documented failures, with sources, are in the
study behind this ADR (summarised; links in "Sources").

| Design | Commit point | Data written | Flushes | Space reserved | Failures that matter here |
|---|---|---|---|---|---|
| AriFS v1 | journal header | 2× | 4 | the journal = the size limit | the limit itself |
| ext4, `data=journal` | jbd2 commit | 2× | 2+ | a transaction is a fraction of the journal | large files are never atomic; applications use temp + rename |
| ext4, `data=ordered` + rename | jbd2 commit | 1× | 2+ | N free | 2009: renames committed before delayed data, empty files after a crash; `auto_da_alloc` is a heuristic that misses cases |
| btrfs, ZFS (copy on write) | superblock, uberblock ring | 1× | 2+ | N free | RAID5/6 write hole (in-place parity under CoW); btrfs log-tree replay bugs; ZFS 2.2 silent zeroed blocks from dirty tracking |
| littlefs | metadata-pair commit with CRC | 1× | per sync | N free | allocator scans O(n²) slow large volumes |
| F2FS atomic write | node write | 1× | 1–2 | N free | atomicity silently dropped under GC pressure |
| FAT, TFAT | none; TFAT: FAT copy | not atomic | several | second FAT | interrupted writes corrupt the volume |
| dpkg, temp + fsync + rename | rename | 1× | 2+ | N free | per-file fsync made installs slow; Pillai et al.: 60 crash bugs in 11 applications |
| SQLite rollback journal / WAL | journal delete / WAL frame | 2× | 2+ / 1+ | journal / WAL | assumes a sector write may tear |

Reusable code: no crates.io file system gives an atomic replace on a block
device in no_std:
- `fatfs` and `embedded-sdmmc`: FAT, no atomicity;
- `ekv` and `sequential-storage`: power-fail safe, but key-value stores
  for NOR flash with unstable formats;
- `littlefs2`: wraps the C library (not built here);
- `littlefs-rust`: a new port, 0.1.0, not yet validated on hardware.

So AriFS stays this system's, improved with what these designs learned:

1. The commit point is one explicit write, after a flush, checksummed
   and sequenced; mount picks the newest valid copy (ZFS's uberblock ring,
   littlefs's metadata pairs).
2. Nothing the committed state references is written in place (the btrfs
   write hole).
3. No heuristics about ordering: the flushes are the contract (ext4 2009).
4. Every write of every operation is cut in a host test (as v1 does), and a
   failed flush fails the operation (PostgreSQL's fsyncgate).

## Options

### Format

- **A. A larger data journal.** v1's mechanism with a journal of the
  largest file (say 16 MiB) and variable-size files.
  - Data written twice, four flushes.
  - The journal reserves the largest file whether used or not.
  - Simplest change of code; the size limit stays, only higher.
- **B. Copy-on-write replace (recommended).** A replace writes the new
  contents to free sectors, flushes, then commits a new generation of the
  table; the old sectors become free only after that commit.
  - Data written once, two flushes.
  - Needs as much free space as the new file, only during the replace.
  - The cost is an allocator; it keeps no state on disk (below), so it
    has nothing to corrupt.
- **C. Adopt a crate:** none fits (above).
- **D. Split packages into 32 KiB files:** pushes the file system's job
  onto every client and does not help data or WebAssembly modules. Rejected.

Writes to replace a 1 MiB file (2048 sectors), from the designs (to be
measured on the branch, see Tests):

| | Sector writes | Flushes | Reserved |
|---|---|---|---|
| v1 (if it allowed 1 MiB) | about 4100 | 4 | 1 MiB journal |
| A | about 4100 | 4 | the largest file |
| B | about 2080 | 2 | none (1 MiB free during it) |

### Layout

- **P1. Grow `ARI0` to the end of the medium on first boot** (systemd-repart:
  only grows the last partition, never moves one). Needs AriBoot or a
  trusted service to rewrite both GPT headers safely.
- **P2. A larger fixed `ARI0`, last on the disk (recommended now).** New
  images put `ARI0` after `ARIBOOT` with 64 MiB. The USB image grows from
  104 to 165 MiB (xz compresses the empty space). Being last, P1 can be
  added later without moving anything. `init` finds `ARI0` by name, so
  the order changes nothing else.

## Proposed decision (B + P2)

1. **AriFS v2 format.**
   - Superblock (LBA 0): magic `ARIFS\0\0\3`, version, sectors, where
     the two table copies are, CRC.
   - Two copies of the table (32 sectors each), each with a
     generation number and a CRC over the whole copy. A commit writes the
     whole new table to the copy not holding the newest generation, then
     flushes. `open` takes the valid copy with the higher generation. A
     torn commit leaves the older copy valid, which is the state before
     the replace.
   - 256 entries of 64 bytes (16 KiB per copy): name (32 bytes, as v1),
     size, and up to 4 extents (start, length).
   - No free-space map: free space is what no entry of the committed table
     covers, computed at `open`. A crash before a commit leaves the new
     sectors unreferenced, which is free.
   - Largest file 16 MiB (`MAX_REPLACE`, `PACKAGE_MAX`); the IPC protocol
     of ADR 0018 is unchanged (chunks, commit, one replace at a time, the
     2-second abandonment).
   - `write` keeps v1's contract (in place, inside the file's extents, the
     size grows only after the data, by a commit).
   - New: `delete`, and `rename` (both one commit), so the installer can
     remove an app.
2. **Migration in place, without copying data.** v1 files are already
   contiguous extents, valid in v2.
   - fsd finishes any pending v1 replace.
   - It writes the two v2 table copies into v1's journal data area (64
     sectors, exactly two copies, unused once the journal is clear),
     flushes, and writes the v2
     superblock. That single sector is the commit: before it the disk is
     v1, after it v2.
   - A migrated disk keeps its 3 MiB; the 64 MiB needs a new stick (or P1
     later).
3. **Rollback stays safe.** An older system must never meet a v2 disk:
   it would not mount `ARI0`, and would lose its keys and its updater
   state.
   - fsd migrates only when both slots hold a system that reads v2 (or
     the other slot is empty). After the first update to a v2 system the
     other slot still holds the old one, so the disk stays v1; it migrates
     after the next update.
   - Once the disk is v2, the updater refuses to install, even with
     `--allow-downgrade`, a system older than the first one that reads v2,
     and says why (ext4's incompatible feature flags, without leaving an
     unbootable state).
4. **Layout P2** in `usb-image` and the test disk; the update image and
   slots are unchanged.

## Tests (for the implementation branch)

- Host: power cuts at every write of replace, delete, rename and
  migration, including a torn table copy; flush errors; allocator
  fragmentation with 4 extents; the 256-entry and 16 MiB limits.
- QEMU: `apps.update` with a 1 MiB package; migration of a v1 disk; an
  update from a v1-only system leaving the disk v1; refusal of a downgrade
  below the v2 reader; `perf` with `replace_1_MiB` against v1's
  `replace_4_KiB` cost per byte.
- The laptop: an update of its stick from 0.17 keeping `authorized_keys`
  and the host key.

## Consequences

- Incident 0026 closes; apps and WebAssembly modules up to 16 MiB.
- Two releases before every machine is migrated (decision 3).
- More code in fsd's trusted path: an allocator and a two-copy table.
  An independent audit is required.

## Sources

- ext4 journal: https://docs.kernel.org/filesystems/ext4/journal.html;
  the 2009 discussion: https://lwn.net/Articles/323106/;
  `auto_da_alloc` misses: https://bugzilla.kernel.org/show_bug.cgi?id=15910
- btrfs status (RAID56 unstable): https://btrfs.readthedocs.io/en/latest/Status.html;
  ZFS silent corruption: https://github.com/openzfs/zfs/issues/15526
- littlefs design: https://github.com/littlefs-project/littlefs/blob/master/DESIGN.md
- F2FS atomic writes: https://lists.openwall.net/linux-kernel/2018/07/15/4
- TFAT: https://learn.microsoft.com/en-us/previous-versions/windows/embedded/aa911939(v=msdn.10)
- dpkg fsync cost: https://bugs.launchpad.net/ubuntu/+source/dpkg/+bug/570805;
  Pillai et al., OSDI 2014: https://research.cs.wisc.edu/adsl/Publications/alice-osdi14.html;
  fsyncgate: https://lwn.net/Articles/752063/
- SQLite: https://www.sqlite.org/atomiccommit.html, https://www.sqlite.org/psow.html
- systemd-repart: https://man.archlinux.org/man/systemd-repart.8
