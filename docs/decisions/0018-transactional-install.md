# ADR 0018: Transactional package installation and update

- **Date:** 2026-09-29
- **Status:** Accepted

## Context

With ADR 0017 an installed program is a signed package at
`apps/NAME.aripkg`, but only xtask knew how to put it there. Stage 4 asks
for an application to be updated from the system itself without data loss
or silent permission escalation, and for a cut during installation not to
leave a half-done state. The atomic AriFS `replace` (ADR 0014) was limited
to one IPC request (4 KiB), smaller than a package.

## Decision

1. **Chunked atomic replace.** `FileServiceProtocol` gains
   `ReplaceChunk(fd, offset, len)` and `ReplaceCommit(fd, len)`: chunks go
   to the journal data area without committing anything; the commit
   computes the CRC of what is in the journal, writes the pending header
   and applies it. `FsClient::replace` chunks only when needed: up to the
   journal capacity (32 KiB) with the same all-or-nothing guarantee. Only
   one replace can be in progress in fsd (`Busy` for the others); it is
   released by its commit, by closing its descriptor or, if its owner
   disappears, by two seconds without chunks (audit 6ee19fb, finding 3).
   A transaction starts at offset 0 and continues without gaps, and the
   commit must cover exactly what it wrote: the library checks this
   (`BadTransaction`, `Invalid` over IPC), so a commit can never pick up
   journal leftovers from another application (audit 6ee19fb, finding 1).
2. **Trusted installer** (`apps/installer`, with the `install` capability
   reserved for it, as `block` is for `updater`): receives fsd's main
   namespace, walks `pkgs/*.aripkg`, verifies each package with the
   development key, reads the installed version from `apps/NAME.aripkg`
   (0 if absent or if it does not verify) and installs only newer versions
   with a single `replace`. Anything that does not verify or is not newer
   is ignored and reported. There is no downgrade: going back requires a
   package with a higher version.
3. **The broker does not change**: it still verifies the package at
   launch, so authority comes from the signature, not from whoever wrote
   the file. The application's data (its private directory in fsd) is not
   touched on update.

## Rejected alternatives

- Writing the new package with `write` and renaming: AriFS has neither
  rename nor delete, and two separate files would double the space.
- Enlarging the IPC message or the client buffer to the package size: the
  guarantee must belong to the file system, not to the size of a message.
- Choosing the package by a numeric shell argument: `run` only passes a
  number; a directory of pending packages is what an update system
  consumes anyway.

## Consequences

- `apps.update`: v1 installed, `pkgs/` with v2 and a v3 with a tampered
  signature; the installer installs 2 and ignores 3; after a reboot 2 is
  still there and installing again does nothing.
- `apps.update_cut`: a cut at the start of installation; on coming back
  the installed package (v1 or v2) launches and the installer completes
  the update.
- An installed package cannot exceed 32 KiB (the journal); SDK programs
  take half of that. Enlarging the journal or chaining several is the
  option when needed.
- An installer that dies with a replace in progress leaves the journal
  busy until fsd restarts; with a single trusted installer this is
  acceptable and noted.
