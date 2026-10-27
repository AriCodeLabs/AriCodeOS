# ADR 0057: Handle values are not reused early, and console input belongs to its reader

- **Date:** 2026-10-27
- **Status:** Accepted (failure-pattern audit 2026-10-25, section "Info";
  incident 0047). Amends ADR 0010, ADR 0013 and ADR 0030.

## Context

ADR 0010 promised that a closed handle never becomes valid again, by
changing the generation of its index. The table handed out the index just
freed first and the 16-bit generation wrapped, so a program that opened
and closed one handle in a loop got a closed value back after 65535
closes (incident 0047, finding 1). ADR 0030 also reasoned that the
packed result of `endpoint_create_pair` never reads as an error because
indexes stay below `HANDLE_LIMIT`.

The console input ring handed whatever was queued to whoever read next:
input typed for a shell that crashed reached the shell init started next,
where it could run as a command (incident 0047, finding 4). Nothing
documented what happens to input left unread.

## Decision

1. **Handle format unchanged**: a `u32` with `generation << 16 | index +
   1`, `0` never valid, and the values of a fresh table unchanged
   (`0x10001`, `0x10002`, ...: the ABI's fixed handles and the handles a
   spawn passes).
2. **Reuse order.** A table of `limit` positions uses every position once
   before reusing one, then the one free the longest (FIFO). Position `p`
   hands out the indexes `p + limit * epoch`; when its generations
   (1..=0xFFFF) run out it moves to its next index, and it starts over only
   after every index of the position has had every generation. A closed
   value comes back only after the table handed out all its other values:
   about 2^32 closes with the kernel's 256 positions.
3. **Indexes are not bounded by the limit.** A live handle's index can be
   anything up to 0xFFFD; only its count is bounded (`HANDLE_LIMIT`). The
   low half of a handle never reads `0xFFFF`, so no handle is
   `0xFFFF_FFFF` and `endpoint_create_pair`'s packed result is never in
   the error range (amends the note in ADR 0030). Programs keep treating
   handles as opaque values.
4. **Console input belongs to its reader.** The process that last called
   `console_read` is the console's reader. When it finishes, the input it
   left unread is discarded, and so is input that arrives before another
   process reads; both are logged with their byte count. A process becomes
   the reader before its pending output (the prompt) is flushed. Input
   typed before anyone has read (at boot) waits for the first reader.

## Alternatives

- **Wider generations** (fewer index bits): changes the handle layout every
  program and ADR 0030 rely on, for a margin the reuse order gives
  without it.
- **Retiring a position for good when its generation wraps**: never
  reuses a value, but a busy table would run out of indexes and fail
  closed after about 2^32 closes, killing a long-running client; starting
  over after the whole value space has the same reuse distance without
  the failure.
- **Flushing the console on every new reader**: would also drop input
  typed ahead for a program the shell starts, and a reader that is alive
  has a right to it.

## Consequences

- A stale handle in a long-running program fails with `BadHandle` instead
  of naming another object, as ADR 0010 meant.
- A table that churns grows to its limit's positions (about 10 KiB of
  kernel heap per process at most: 256 entries of 32 bytes and the free
  queue) instead of reusing one slot.
- A crashed shell's half-typed line is gone; the user types it again at
  the new prompt.
