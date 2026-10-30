# ADR 0066: Adopted cryptography, measured against ours

- **Date:** 2026-10-30
- **Status:** Accepted. Step 3 of ADR 0063's order of work. On 2026-10-30
  the owner chose to adopt the winners everywhere, the kernel and AriBoot
  included, after seeing the measurements below.

## Context

`aricode-crypto` was written from scratch (Phase 31, then TLS and SSH).
Its defects were the kind established libraries fixed years ago
(incident 0044): Ed25519 verification accepted forged signatures, AES used
lookup tables.

ADR 0063 asks for every subsystem to be compared with the established
implementations, on compatibility and on how fast the system runs, keeping
the winner. The candidates were RustCrypto (`sha2`, `aes-gcm`,
`chacha20poly1305`, `p256`, `p384`, `rsa`) and the dalek crates
(`ed25519-dalek`, `x25519-dalek`): maintained, audited (NCC Group 2020 for
the AEADs, Quarkslab 2019 for dalek), `no_std`, under MIT, Apache-2.0 or
BSD-3-Clause.

## Measurements

### Compatibility

Both implementations ran the same Wycheproof vectors (C2SP/wycheproof,
commit d17cc89), about 6,000 cases:
- AES-GCM and ChaCha20-Poly1305;
- Ed25519 and X25519;
- ECDSA P-256 and P-384;
- RSA PKCS#1 v1.5 and PSS (2048 to 4096 bits);
- HMAC and HKDF.

**Neither gave a single wrong answer.** They differ only in what they
cover: RustCrypto also does AES-192, other IV lengths, PSS salts other
than the hash length, and HMAC-SHA-512. TLS 1.3 and SSH as AriCodeOS
speaks them need none of these.

### Speed

Native, on the reference host (AMD Ryzen 7 5800X). The same code is built
the two ways AriCodeOS builds:
- soft-float, as the kernel and AriBoot are, with the portable backends;
- with SSE, as user programs are (ADR 0065), with the CPU's extensions
  detected at run time.

Best of five; values are ours / adopted.

| | Soft-float | With SSE |
|---|---|---|
| SHA-256, 16 KiB | 298 / **392** MB/s | 348 / **2392** MB/s (SHA-NI) |
| SHA-512, 16 KiB | 453 / **634** MB/s | 455 / **807** MB/s |
| AES-128-GCM, 16 KiB | 25 / **171** MB/s | 25 / **908** MB/s (AES-NI) |
| ChaCha20-Poly1305, 16 KiB | 269 / **495** MB/s | 284 / **1891** MB/s |
| Ed25519 verify | 190 / **46** µs | 214 / **29** µs |
| Ed25519 sign | 217 / **44** µs | 238 / **25** µs |
| X25519 | 59 / **38** µs | 63 / **37** µs |
| ECDSA P-256 verify | **128** / 191 µs | **128** / 184 µs |
| ECDSA P-384 verify | **418** / 806 µs | **418** / 810 µs |
| RSA-2048 verify | **126** / 231 µs | **96** / 230 µs |

Our ECDSA and RSA verification win. They only ever handle public values,
so they can take shortcuts that `p256` and `rsa`, which also sign, do not.

Without SHA-NI (the reference laptop's Ivy Bridge has none), `sha2`'s
portable code still beats our SHA-256: 406 against 336 MB/s with SSE.

## Decision

1. **Adopted, everywhere:**
   - SHA-256, SHA-384 and SHA-512: `sha2`;
   - AES-GCM: `aes-gcm`;
   - ChaCha20-Poly1305: `chacha20poly1305`;
   - ChaCha20 and Poly1305 on their own (the SSH cipher, the kernel
     generator's blocks): `chacha20` and `poly1305`;
   - Ed25519: `ed25519-dalek`, with precomputed tables;
   - X25519: `x25519-dalek`.

   Our implementations of these are deleted (`aes.rs`, `field25519.rs`
   and the internals of the rest).
2. **Kept, ours:**
   - ECDSA P-256 and P-384 verification and RSA verification, which
     measured faster;
   - HMAC and HKDF: a few lines over the hash, tied with the alternatives,
     so no crate is added for them;
   - the kernel generator (`chacha20::Rng`), the OpenSSH cipher
     (`ssh_chachapoly`) and `trust`, which are protocols, not primitives.
3. **`aricode-crypto` stays the one interface.** Callers do not change,
   and its policy sits on top of whatever implements it:
   - **Ed25519 verification** is `verify_strict` plus a check of ours: the
     public key's y must be below p. `ed25519-dalek` accepts a
     non-canonical encoding of a large-order key, which our old verifier
     refused. Small-order A or R and S ≥ L are refused, as since incident
     0044.
   - **Key material is wiped when dropped, on every backend.**
     - `sha2` 0.10 cannot zeroize.
     - The run-time detected backend of `polyval` (GHASH, inside
       `aes-gcm`) keeps its key in a `ManuallyDrop` that is never dropped.
     - The facade's `Drop` therefore zeroes those states itself
       (`wipe_state`). Their layouts were checked to be valid as zeros
       (no reference, `NonZero`, enum or `Option`).
     - `sha2`, `aes`, `aes-gcm`, `ghash` and `polyval` are pinned with `=`,
       so a version bump has to check that again.
   - **What the adopted code leaves on the stack is scrubbed.**
     - `ed25519-dalek` leaves the signing nonce r and the secret scalar
       there; with r and a signature, anyone recovers the private key.
       `x25519-dalek` leaves the clamped scalar and the ladder, and `sha2`
       its message schedule (derived from HMAC keys). Our old code wiped
       all of these.
     - Signing, the public key from a seed, X25519, HMAC and AES key
       expansion run in a frame of their own (never inlined). Then a scrub
       zeroes the stack below it with volatile stores.
     - The scrub is 4 KiB after curve operations and 2 KiB after HMAC and
       key expansion. Painting the stack measured at most 2.5 KiB and 1.1
       KiB, in both builds.
     - It writes 8-byte words. Natively it costs nothing measurable:
       X25519 takes 37.4 µs with it and without it.
   - **`chacha20::block` uses the cipher core.** The crate's stream
     wrapper refuses the last block of a nonce (2^32 − 1), which the
     kernel generator uses when it reseeds: the kernel would have
     panicked there.
4. **No SIMD in the trusted base.**
   - The kernel and AriBoot enable `aricode-crypto/soft` (`sha2`'s portable
     code), and `.cargo/config.toml` forces the portable backends of
     `aes`, `polyval`, `chacha20`, `poly1305` and `curve25519-dalek` for
     their targets.
   - `cpufeatures` already answers "no" there ("freestanding and UEFI
     targets do not support SIMD features"). The flags make it explicit
     instead of resting on that crate.
   - Every OS build disassembles the kernel and AriBoot and refuses either
     if:
     - it names a vector register;
     - it uses an instruction that changes vector state without naming one
       (`vzeroupper`, `vzeroall`, `ldmxcsr`, `femms`).

     This is `check_no_simd`. Injecting a `pxor xmm0, xmm0` into the
     kernel fails the build.
   - Their builds drop `RUSTFLAGS` and its variants from the environment,
     which would replace the target's flags.
5. **Supply chain** (ADR 0063, point 4).
   - 36 crates are added to `Cargo.lock`, all from crates.io, pinned with
     their checksums, under accepted licences.
   - Two of them are never built for AriCodeOS's targets: `libc`, which
     `cpufeatures` names only for aarch64 Linux, and `fiat-crypto`, an
     unused `curve25519-dalek` backend.
   - Build scripts and procedural macros run at build time on the host;
     none is linked.
   - **Kernel:** only two build scripts, `curve25519-dalek` (it picks the
     backend from the `--cfg`, through `rustc_version` and `semver`) and
     `generic-array` (rustc version, through `version_check`). No
     procedural macro.
   - **AriBoot:** the same two, plus what it already had (`uefi-macros`
     and `ptr_meta_derive`, with `proc-macro2`, `quote` and `syn`).
   - **User programs** add `curve25519-dalek-derive`, the procedural macro
     of the AVX2 backend.
   - `x25519-dalek` is used through its bare `x25519` function only, so
     its `zeroize` feature (which brought `zeroize_derive`) is off.
   - The check of user programs' dependencies now filters the graph for
     their target (`cargo metadata --filter-platform`): a `cfg(unix)`
     dependency is still refused, an aarch64-only one is not counted.
6. **Wycheproof stays as a test** (`libs/crypto/tests/wycheproof.rs`, 2.6
   MB of vectors with only the fields the test reads, regenerated by
   `tools/wycheproof-extract.py`).
   - Every case the parameters cover must get the expected answer.
   - Each file must judge a minimum number of cases.
   - A version bump of an adopted crate runs through it.
   - `cargo xtask test --level unit` runs `aricode-crypto`'s tests twice:
     - with the host's run-time detected SIMD backends;
     - with the portable ones and the flags the kernel and AriBoot build
       with, in a target directory of their own.

## Consequences

- In the guest (QEMU TCG, `cargo xtask perf`, 1 CPU, medians), against
  the same build before:

  | Figure | Before | After |
  |---|---|---|
  | ChaCha20-Poly1305, 16 KiB | 195 µs | 102 µs |
  | AES-128-GCM, 16 KiB | 2412 µs | 655 µs |
  | Ed25519 verify | 1289 µs | 334 µs |
  | X25519 | 370 µs | 308 µs |
  | SHA-256, 16 KiB | 159 µs | 222 µs |

  - SHA-256 is slower only under TCG. `qemu64` has no SHA-NI, and TCG
    emulates `sha2`'s portable code worse than ours, while the hardware
    runs it 20 to 29 % faster (above).
  - The reference laptop measures it on hardware before the next release.
- Binaries grow: each algorithm carries its portable and its SIMD backends,
  and Ed25519 a 30 KiB table of precomputed points. Stripped:

  | Binary | Before | After |
  |---|---|---|
  | AriBoot | 113,152 | 156,160 |
  | kernel | 926,280 | 938,568 |
  | shell | 406,088 | 520,784 |
  | updater | 410,216 | 529,008 |
  | sshd | 111,176 | 238,160 |
  | initrd | 2,637,728 | 3,364,224 |

  The initrd stays far below the 12 MiB of a system image.
- The deepest stack path of `shell` and `updater` grows from 26.4 to 29.1
  KiB, of a 32 KiB budget (incident 0018): Ed25519 verification's frame
  is 5.5 KiB.
- `aricode-crypto` now has dependencies. Its description and module
  documentation say which implementation is behind each module.

## Audit (2026-10-31)

An independent audit found no high findings. It confirmed:
- the Ed25519 policy matches the old one;
- AEAD `open` leaves data untouched on failure (aes-gcm 0.10.3 includes the
  fix for CVE-2023-42811);
- the panics are reachable only past 64 or 256 GiB;
- the kernel `Rng` is correct at block 2^32 − 1;
- no SIMD reaches the trusted base;
- the facade adds no secret-dependent branch.

Its findings and what was done:

- **M1.** Secrets stayed on the stack where our old code wiped them: the
  signing nonce, the secret scalar, X25519's scalar and ladder, `sha2`'s
  message schedule.
  - Fixed with the frame isolation and scrub of decision 3.
  - A host test searches the stack after each operation for the seed,
    the expanded key, the nonce hash, the reduced scalar a and the nonce
    r (computed independently), X25519's clamped scalar and HMAC's
    padded keys.
  - Without the scrub it finds r after signing, in both profiles. With
    it, nothing is found.
  - Two mistakes were found on the way. The scrub first left `memset`'s
    return address below itself; it now uses volatile stores on an
    uninitialized area. The secret code was inlined into the caller's
    frame, above the scrubbed area; it now runs in a frame of its own.
- **L2.** The `wipe_state` safety argument was partly wrong: `poly1305`
  wipes itself on every backend, and its AVX2 state holds `Option`s.
  - The facade no longer touches it; `Poly1305` is now just `tag`.
  - The layouts relied on are listed in the safety comment, and those
    crates are pinned with `=`.
- **L3.** The portable backends the trusted base runs were tested only in
  QEMU. They now run in the host tests too (decision 6).
- **L4.** The vector-register guard missed instructions that name no
  register. They are refused now (decision 4).
- **L5.** `RUSTFLAGS` in the environment would have replaced the kernel's
  flags. It is removed for those builds (decision 4).
- **L6.** An unused `x25519-dalek` feature brought a procedural macro into
  the kernel graph, and the list of build scripts was inaccurate. Both
  are corrected (decision 5).
- **L7.** `Poly1305::update_padded` and `finish` were dead code. They are
  removed.

