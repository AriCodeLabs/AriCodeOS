# ADR 0010: Kernel objects by handle with generation and rights

- **Date:** 2026-09-26
- **Status:** Accepted

## Context

Section 24 of the master document requires that applications never handle
pointers or global identifiers of kernel objects, and section 25
that access depends on an explicit capability.

## Decision

Each process has a handle table (`libs/handles`) with a fixed limit. A
handle is an opaque `u32` with an index (16 bits) and a generation (16 bits); `0`
is never valid. Each entry holds an `Arc<KernelObject>` and some `Rights`.
The rules:

1. A closed handle never becomes valid again even if its index is reused
   (the generation changes).
2. A made-up handle would only be valid if it guessed the index and generation of
   a live entry.
3. Duplicating requires the `DUPLICATE` right and can only reduce rights.
4. Every syscall that operates on an object declares the right it requires
   (`console_write` -> `WRITE`, `object_id` -> `READ`).
5. The informational identifiers (tid, pid) returned by `object_id` do not
   grant access: only the handle does.

Every process starts with three fixed handles: its process, its main thread and
the console (`sdk/abi::handle`).

## Alternatives

- Global identifiers (thread numbers, process numbers) as syscall
  arguments: anyone could name any object; rejected.
- Handles without a generation: a closed and reassigned handle would grant access to
  someone else's object; rejected.

## Consequences

- The ABI moves to version 2: `console_write` takes a handle.
- Phase 13 (capabilities) extends this model with delegation between
  processes (`TRANSFER`) and revocation, without changing the representation.
- The tables live in the kernel and count against the process limit
  (section 52).

## Note (2026-10-27, ADR 0057)

Rule 1 did not hold over time: the index just freed was reused first and
the 16-bit generation wrapped, so a closed handle came back after 65535
closes (incident 0047). ADR 0057 keeps the format and changes the reuse
order: every position is used before one is reused, oldest free first,
and a position whose generations run out moves to another index, so a
value comes back only after about 2^32 closes in the same table.
