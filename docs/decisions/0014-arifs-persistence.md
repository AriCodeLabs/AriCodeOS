# ADR 0014: AriFS persistence: checksums, replace journal and `flush`

- **Date:** 2026-09-28
- **Status:** Accepted

## Context

AriFS v0 wrote in place with no checksums and no `flush`: a power cut
during a write could leave a mixed file, broken metadata passed as valid,
and nothing guaranteed that what was written had reached the medium. The
2026-09-28 study (stage 2) asks for an explicit contract for `flush`,
partial writes, atomic replacement and recovery, and for choosing between
a journal and copy-on-write in an ADR. A/B protects the system, not
application data.

## Decision

1. **Format v1 with CRC32** in the superblock and in every in-use table
   entry. `open` returns `Corrupt` if any of them does not match, and fsd
   does not mount: a file system that cannot be trusted is neither served
   nor "repaired" by deleting. There is no compatibility with v0 (it was a
   prototype; xtask reformats).
2. **Replace journal** (one header sector and 64 data sectors, between the
   table and the data) instead of copy-on-write: files have fixed capacity
   and no fragmentation, so a single transition block is enough and the
   format barely changes. `replace(fd, data)` writes the data and its CRC
   to the journal, `flush`, marks the header pending with its own CRC,
   `flush`, copies to the file, sets the size, `flush`, clears the header
   (without `flush`: a lost clear only costs redoing an already applied
   replace, and every `flush` is an fsync of the medium; stage 5). On
   mount, a valid pending header with matching data is replayed
   (idempotent); a broken header or mismatching data is discarded: the
   replace was never committed and the file keeps its previous contents.
3. **`write` contract**: writes in place, sector by sector, does a `flush`
   and only then grows the size. After a cut each sector holds either the
   old or the new contents; there is no guarantee across sectors; the size
   never covers data that is not durable. Whoever needs all-or-nothing
   uses `replace`. Likewise `create` makes the superblock reservation
   durable before publishing the entry, because the medium may reorder
   sectors until `flush` (audit 6ee19fb, finding 2), and `open` rejects
   (`Corrupt`) metadata with a valid CRC that is inconsistent: files
   outside the allocated area or overlapping.
4. **End-to-end `flush`**: `virtio-blk` negotiates `VIRTIO_BLK_F_FLUSH` and
   handles `Op::Flush` of the block protocol (without it, `Flush` returns
   without doing anything and the driver announces this at startup);
   `BlockDevice::flush` carries it to the library; the file protocol gains
   `Flush` and `Replace`, and the SDK gains `FsClient::{flush, replace}`.
5. **Medium assumption**: a 512-byte sector is written entirely or not at
   all. Without that assumption no scheme with a single copy of the header
   is safe; it is documented for the reference machine.

## Rejected alternatives

- **Copy-on-write with two copies per file**: doubles the data space
  (encroaches on the raw area of the test disk) and still needs an atomic
  pointer; it adds nothing over the journal with fixed capacity.
- **General journal for partial writes**: multiplies the writes of every
  `write` and complicates replay; the stage's use case (saving a whole
  file) is covered by `replace`.
- **Repair on mount** (discard corrupt entries and carry on): hides losses;
  failing visibly and leaving the decision to the operator is preferred.

## Consequences

- The `libs/arifs` host tests cut a replace at every write, with the cache
  lost or kept, and check that the file stays whole (old or new) and that
  the file system keeps serving. They also cover superblock and table CRCs,
  a broken journal and mismatching data.
- `filesystem.replace_recovery` (disk `fs-pending-replace`) and
  `filesystem.corrupt_detected` (disk `fs-corrupt`) test it in QEMU;
  `filesystem.read_write` requires the negotiated `flush` and a `replace`
  over IPC.
- `replace` over IPC accepts up to `MAX_IO` (4 KiB) per request: the size
  of a note, not of a module; long writes are still `write`.
