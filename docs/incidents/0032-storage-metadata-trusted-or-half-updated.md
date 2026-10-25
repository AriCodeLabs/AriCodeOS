# Incident 0032: storage metadata trusted unbounded, or left half-updated

- Opened: 2026-10-25. Status: fixed on `fix/storage-bounds`.
- Found by: the failure-pattern audit of 2026-10-25 (audit B, M1 and L1;
  audit C, M2). Not seen on a machine.
- Environment: any disk the machine sees (an inserted stick is enough
  for the loader) and the AriFS data partition.

## Evidence

- GPT (B, M1): `Header::entries_bytes` multiplied the header's
  `entry_count` (any `u32`, covered only by the header's own CRC) by 128
  and the loader, init and the updater allocated that much to read the
  table. A stick whose header says `0xFFFFFFFF` asked for about 512 GiB:
  AriBoot aborts on the failed allocation, so the machine does not boot
  while that stick enumerates before the system disk. `Partition::sectors`
  underflowed for an entry that ends before it starts.
- AriFS (C, M2, reproduced by the audit on the host): `create` put the
  new entry and `data_next` in memory before writing them; a failed write
  returned `Io` with the entry still live. A later `write` to it
  persisted the entry over a superblock that never reserved its space, and
  the next mount refused the volume as corrupt. `write`, `apply_journal`
  and `truncate` likewise changed the size in memory before the table.
- AriFS (B, L1): the layout check added a forged entry's start and
  capacity without overflow checks (debug builds panic, release builds
  wrap past every bound), a capacity whose bytes do not fit `u32`
  overflowed `capacity_bytes`, and `recover` replayed a pending journal
  longer than the journal area, i.e. other files' sectors, into the
  target file. fsd added the partition base to an LBA unchecked.

## Fix

- GPT: at most `MAX_ENTRIES` (1024) entries, at least one; a partition
  that ends before it starts is refused (`GptError::BadEntry`); the
  header length is bounded by the sector size.
- AriFS: `store_entry`/`store_size` write first and take the new metadata
  in memory only once the write succeeded; the superblock likewise.
  Layout arithmetic is checked; `recover` bounds a journal by the journal
  area as `commit_header` does; fsd's LBA addition is checked.
- Host tests: `entry_count_is_bounded`,
  `a_partition_ending_before_it_starts_is_refused`,
  `failed_metadata_writes_leave_memory_as_the_disk`,
  `layout_arithmetic_is_checked`,
  `a_journal_longer_than_its_area_is_discarded`.

## Audit of the fix

- M1: a write failing after a replace was committed (the header may have
  landed, or the apply broke off) used to leave the session reading a
  mix, and a later write was lost when the next mount replayed the
  journal. The volume is now poisoned: every operation fails until it is
  opened again (`needs_reopen`), and fsd exits so its supervisor
  relaunches it and the mount redoes the journal. Test
  `a_failed_apply_poisons_until_reopened`.
- M2: a 1024-entry table (128 KiB) in init's fixed 256 KiB heap could
  still abort it. `aricode_gpt::TableScan` reads a table piece by piece
  with a running CRC and no buffer; the SDK's `block::find_partition`
  (now shared by init and the updater) and virtio-blk's report use it.
  Every used entry must lie within the header's usable range (which
  bounds `Partition::sectors`, L4), not only the first ones that fit.
  Tests `table_scan_takes_any_split` and the range cases.
- L1, L2, L6: the layout test exercises the capacity bound; `create_sized`
  refuses a capacity that would not mount again and checks `data_next`
  arithmetic.
- Open (L3): the superblock's `total_sectors` is not checked against the
  partition's real length; fsd is given only the partition's first LBA.
  Passing the length changes fsd's launch arguments: a follow-up.
