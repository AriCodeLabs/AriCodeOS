# ADR 0017: Out-of-tree applications, packaged and installed in AriFS

- **Date:** 2026-09-29
- **Status:** Accepted

## Context

Until now every program lived in the initrd: adding an application meant
touching the workspace and regenerating the image. Stage 4 of the plan
(2026-09-28 study) asks for an application developed out of tree to be
built, installed, run and updated, with identity by publisher and
content, a signed manifest and transactional installation.
`process_spawn` only accepted initrd names.

## Decision

1. **Package = one signed file** (`aricode-pkg`): an `aricode-initrd`
   container with `manifest` (name, version, SHA-256 of the program and of
   its application manifest), `manifest.sig` (Ed25519 over the manifest,
   with the development key as for system images), `program` (ELF) and
   `program.manifest`. Once installed it is `apps/NAME.aripkg` in fsd's
   main namespace. The application manifest travels inside the package and
   is therefore signed: an installed program cannot ask for more than an
   initrd program with the same manifest.
2. **A single file, atomic installation**: installing or updating is an
   AriFS `replace` (ADR 0014); after a cut there is either the previous or
   the new version, never a mix of program and manifest.
3. **The broker verifies at launch.** If the program is not in the initrd,
   it reads `apps/NAME.aripkg` into a DMA staging buffer (256 KiB,
   allocated once; `PACKAGE_MAX`, 32 KiB, since incident 0026), verifies the signature and hashes, applies the same
   policy to the application manifest and creates the process with
   `process_spawn_from` directly from the buffer (offset and length of the
   ELF). A package that fails verification is not launched (`Denied`, with
   the reason on the console).
4. **New syscall `process_spawn_from` (30)**: like `process_spawn`, with
   the ELF in a DMA buffer of the caller (`spawn_from::Request`: handle,
   offset, length, name). It requires `MANAGE` on the `SPAWNER` and `READ`
   on the buffer; the kernel copies the segments when creating the process
   and the buffer can be reused afterwards. The ELF is parsed with bounds
   checking over memory the caller might be modifying: at worst that
   produces an invalid process for the caller itself, never a kernel fault.
5. **Out-of-tree build**: `cargo xtask app-pack DIR [--version N]` builds
   `DIR/Cargo.toml` (its own workspace, SDK by path) from the repository
   root, so `rust-toolchain.toml` and `.cargo/config.toml` apply just as
   they do to initrd programs, sets `APP_VERSION` at build time, signs and
   leaves `build/pkgs/NAME-N.aripkg`. A scenario declares
   `install = ["DIR"]` and xtask installs it on the test disk (AriFS
   supports files whose capacity is a multiple of 64 sectors:
   `create_sized`).

## Rejected alternatives

- Loose ELF and manifest in AriFS (`apps/NAME` + `apps/NAME.manifest`):
  unsigned and with two files to update, a cut could leave a new program
  with an old manifest.
- Verifying only at install time: anyone who can write to AriFS could
  replace the program afterwards; verifying at launch makes authority
  depend on the key, not on disk access.
- Mapping the installed ELF directly from disk or passing it through
  `copy_from_user`: AriFS does not provide pages, and the large copy ties
  the size to init's heap; the DMA buffer is already contiguous memory
  reachable through the physical map.

## Consequences

- `apps.external`: `hello_ext` is built from `examples/hello_ext`,
  packaged with version 1, installed on the test disk, and `run
  hello_ext` launches it twice with its manifest.
- The installed package is bounded by the AriFS journal (32 KiB) if it is
  to be updated atomically, and by the staging buffer (256 KiB) to be
  launched; SDK programs take tens of KiB. (Incident 0026: the two bounds
  let a package run that nothing could update; since then one limit,
  `aricode_pkg::PACKAGE_MAX` = the journal, holds for launching, staging,
  installing and building.)
- Installing and updating from inside the system (a trusted installer,
  packages with more than one version, rejecting bad signatures and cuts
  during installation) is the next part of the stage.
