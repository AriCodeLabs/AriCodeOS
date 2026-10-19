# ADR 0016: Per-resource permissions: read-only shared files

- **Date:** 2026-09-28
- **Status:** Accepted

## Context

The manifest (Phase 28) grants capabilities per service: `fs` gives a whole
private directory. There was no way to give an application access to a
specific file elsewhere, or to give it read-only. The 2026-09-28 study
(stage 3, section C) asks to start with "a selected file in read mode",
with the restriction enforced by the server, not by the manifest: writing
the permission down is not enough to make it effective.

## Decision

1. **`read_files = a, b` in the manifest** (up to 4 full AriFS names). The
   broker asks fsd for one endpoint per file (`Op::Share`, only through the
   main endpoint) and hands them over after the private directory, in
   manifest order. If the file does not exist, the launch is denied before
   the process is created.
2. **fsd enforces the restriction.** A shared endpoint is a single-file
   namespace: `List` shows only that file (with its name unprefixed),
   `Open` opens only that file, `Read` and `Close` work, and `Create`,
   `Write`, `Replace`, `Flush`, `Bind` and `Share` answer `Denied`.
   Descriptors still belong to the endpoint that opened them (SDK audit).
3. **Authority travels with the object**, not with the identity: two
   applications with the same manifest receive duplicates of the same
   shared endpoint; one with a private directory and another with a shared
   file receive different objects. Closing or losing one's endpoint does
   not affect the others.

## Rejected alternatives

- Grants by client identity in fsd: requires fsd to know who is calling
  and a duplicated policy; the capability model already expresses the
  permission with the endpoint.
- A read-only flag on `Bind` (whole directory read-only): broader than
  what the stage asks for and does not cover "a file from elsewhere".

## Consequences

- `sandbox.shared_file`: `notes_reader` (manifest `read_files =
  hola.txt`) lists and reads the file, gets `Denied` when writing and does
  not see `README`; `hello_app` in the same session keeps its private
  directory.
- Writing to a shared file, sharing whole directories and a file picker
  for granting at session time are later stages; the mechanism (`Share`
  with a per-endpoint restriction) is the same.

## Note (ADR 0036, 2026-10-19)

A `Bind` or `Share` endpoint is no longer shared by every launch of the
same name: each grant is a session of its own, of which fsd keeps only the
receive side, so that the kernel can report the client gone and fsd release
its descriptors (incident 0011). The restriction each kind enforces is
unchanged; a third kind, `Attach`, gives the whole namespace to the
`install` holder and the shells.
