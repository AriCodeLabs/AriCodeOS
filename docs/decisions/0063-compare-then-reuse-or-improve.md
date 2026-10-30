# ADR 0063: Compare, then reuse or improve

- **Date:** 2026-10-29
- **Status:** Accepted. The owner set the direction on 2026-10-29: the
  licences, the order of work, and the update-download choice left to
  this ADR's author.

## Context

"From scratch" (`AriCodeOS.md`, section 1) was read as "write everything
ourselves". That is how the project ended up with its own cryptography,
TLS, X.509, SSH, TCP/IP and HTTP. Most of the serious findings of the
last releases came from that code, and they were mistakes that
established implementations fixed years ago:

- Ed25519 verification accepted 1160 of 12 544 forged signatures
  (incident 0044);
- AES used lookup tables, which leak through timing (incident 0044);
- certificates were matched by their common name (ADR 0058);
- wildcards were accepted over a public suffix (ADR 0062).

Writing each piece also slowed the project.

The owner's meaning of "from scratch", in their words (2026-10-29):

- it is not "create everything";
- look at what exists, compare it, take ideas from others' failures, and
  improve on them;
- do not copy something just because it was written years ago and works,
  when nobody put it to the test or thought of something better;
- create, compare with the old, make it compatible, improve it.

## Decision

1. **Every subsystem is compared with the established implementations.**
   Before writing a subsystem or changing one deeply:
   - study the established implementations (BSD, Linux, maintained
     libraries) and their failure history: CVEs, audits and users'
     complaints;
   - then either adopt one or write ours, informed by them.

   Age alone is no reason to trust code. Being ours is no reason to keep
   it either.

2. **Measured, not assumed.** Our implementation and the established one
   run the same tests:
   - **compatibility:** published test vectors, and interoperability with
     the reference tools (OpenSSL, OpenSSH, real servers);
   - **speed:** how fast the OS runs, measured inside AriCodeOS on the
     perf scenarios, not how fast the code is to write. The owner prefers
     a faster and better system over faster development.

   The ADR of each subsystem records the measurements. It keeps the
   implementation that wins, or says why it keeps the slower one.

3. **Licences.** Code adopted from elsewhere is under one of these
   licences:
   - 0BSD, Apache-2.0, BSD-2-Clause, BSD-3-Clause, BSL-1.0, CC0-1.0, ISC,
     MIT, Unicode-3.0, Unicode-DFS-2016, Unlicense or Zlib;
   - MPL-2.0, whose copyleft stays in the files it covers (as with the
     Public Suffix List extract, ADR 0062).

   Not accepted:
   - GPL, LGPL and AGPL;
   - proprietary code (Windows code is never usable).

   Our own new drivers carry the project's licence. ADR 0042's licence
   question is set aside: no GPL driver code is adopted.

4. **Supply chain.** Third-party crates are kept on a short leash:
   - they come from crates.io only (no git or path sources);
   - `Cargo.lock` is committed and pins every version with its checksum;
   - `cargo xtask check` runs `cargo metadata --locked`, so an
     out-of-date lock is refused;
   - only the features a crate is needed for are enabled;
   - a new crate, or a version bump, goes through a branch where its
     source diff is read, with build scripts and procedural macros named
     in the ADR;
   - code linked into the kernel or AriBoot needs the strongest reason,
     since it enters the trusted base.

5. **What stays ours.** These are the project's own design, kept and
   compared like everything else:
   - the microkernel, capabilities, IPC and the driver isolation model;
   - the update system's rules.

6. **Order of work.**
   1. Update downloads larger than the updater's memory (streamed into the
      inactive slot, ADR to follow). Adopted code makes images grow.
   2. SIMD in user programs: the kernel saves the FPU/SSE state eagerly on
      every switch (the lazy scheme was CVE-2018-3665, LazyFP). Without it
      no implementation can use AES-NI, SHA-NI or vector code. Done in
      ADR 0065.
   3. Cryptography: compare `aricode-crypto` with RustCrypto and the dalek
      crates.
   4. TLS and X.509: compare with rustls and rustls-webpki.
   5. SSH: compare with sunset and Dropbear's design.
   6. TCP/IP, then the WASM interpreter: compare with smoltcp and wasmi.

## Consequences

- `cargo xtask check` checks third-party crates (`tools/xtask/src/deps.rs`):
  - it refuses a crate that does not come from crates.io, a crate without
    a declared licence, and a licence expression that does not reduce to
    accepted licences;
  - the rules for combining licences: `OR` needs one side accepted, `AND`
    needs both, and `WITH` an exception needs the licence it follows;
  - at the time of writing it covers 31 crates, all used by AriBoot or
    xtask.
- Binaries grow as code is adopted. The 6 MiB download limit (incident
  0021) and the 32 KiB package limit (incident 0026) get pressing; the
  first is step 1 above.
- `AriCodeOS.md` section 1 states the meaning of "from scratch" given
  here.
