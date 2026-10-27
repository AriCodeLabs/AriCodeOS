# Incident 0044: crypto side channels and secret lifetime

- Opened: 2026-10-27. Status: fixed on `fix/crypto-hardening`.
- Found by: the failure-pattern audit of 2026-10-25 (section "Info":
  "secrets are not zeroized; `ct_eq` has no optimization barrier; AES uses
  tables; Ed25519 verify accepts small-order keys"). Not seen on a machine.
- Environment: `libs/crypto` and its users (`libs/tls`, `libs/ssh`,
  `sdk/https`, `services/sshd`); every Ed25519 check (release key and
  package signatures, update index, SSH public-key login, X.509 and TLS
  CertificateVerify).

## Evidence

1. **Ed25519 accepted small-order keys and commitments** (reproduced).
   A host test on `libs/crypto` at `5ef2f6e` built signatures with no
   private key: A and R taken from the eight points of order dividing 8
   (canonical encodings, "Taming the many EdDSAs", table 6), from the
   three non-canonical encodings with y >= p that the decoder reduced
   (y = p, both signs, and y = p + 1) and from three that it already
   refused; S = 0; messages 0..63. `verify` accepted 1160 of the 12 544
   (A, R, message) triples. With A = identity (`01 00 .. 00`) and
   R = identity, it accepted every message; with A = `ee ff .. 7f` (the
   identity written with y = p + 1) the same. The equation
   [S]B = R + [k]A holds for S = 0 whenever R + [k]A is the identity,
   which for a small-order A happens for one message in eight or better.
2. **AES used a table S-box** (by inspection). `SubBytes` and the key
   expansion indexed a 256-byte table with key- and data-dependent bytes
   (`SBOX[old[..] as usize]`): which cache line is loaded depends on the
   secret, the classic cache-timing leak (Bernstein 2005, Osvik-Shamir-
   Tromer 2006). The module said so ("not hardened against cache-timing
   side channels"). GHASH (`aead.rs`) was checked too: it multiplies bit
   by bit with masks, no table and no secret-dependent branch, and needed
   no change.
3. **`ct_eq` had no optimization barrier** (by inspection). The OR
   accumulator was plain arithmetic: nothing stopped the optimizer from
   noticing that once `diff` is nonzero the result is `false` and exiting
   early, which would make tag and signature comparisons leak the position
   of the first mismatch through timing. The code generated today was not
   seen to do it; the point is that nothing forbade it.
4. **Secrets were not wiped** (by inspection). `wipe` was used for the
   boot and kernel seeds only. Everything else stayed in memory after use:
   the sshd host key in each connection's `Server`, the hex text of the
   host key and of `random_seed`, Ed25519's expanded key, scalar and
   nonce, X25519's clamped scalar and ladder, the TLS ephemeral key (kept
   in `Client` for the life of the connection, after the ServerHello had
   used it), the shared secrets, HKDF and HMAC intermediates (padded keys,
   inner digests, the last `T(i)`), TLS traffic secrets and record keys,
   SSH session keys and the hash input holding K, the AES key schedule and
   GHASH key, Poly1305 one-time keys, the hash states that had absorbed a
   secret and the TLS and SSH generators seeded per connection.

## Mechanism

1. RFC 8032's verification equation does not exclude small-order points,
   and decoding (section 5.1.3) says "if the resulting value is >= p,
   decoding fails", which `decompress` did not check: `Fe::from_bytes`
   reduced y modulo p. A public key no seed can produce (a clamped scalar
   times B has order L) was accepted, and with it signatures nobody made.
   Every caller verifies with a key it trusts (embedded release and
   development keys, owner-installed `authorized_keys`, keys vouched for by
   a certificate chain), so an exploit needs a small-order key in one of
   those places; an `authorized_keys` line with one would have let anyone
   log in.
2. A table indexed by secret bytes.
3. `diff |= x ^ y` is pure; the comparison loop was constant time only by
   the optimizer's choice.
4. No owner for the lifetime of secret buffers: they were plain arrays and
   structs, and memory returned to the stack or the heap kept its content.

## Fix

1. Ed25519 (`libs/crypto/src/ed25519.rs`): `decompress` refuses an
   encoding whose y is not below p (it must survive `from_bytes` /
   `to_bytes` unchanged); `verify` refuses a public key A or a commitment R
   with [8]P = identity (`Curve::is_small_order`: three doublings, then
   x = 0 and y = z; [8]P has order 1 or L, and the only other point with
   x = 0 has order 2). Tests: `small_order_points_are_found` and
   `small_order_and_non_canonical_keys_and_commitments_are_refused` (the
   evidence's 11 decodable encodings as A and as R, 64 messages each, none
   verifies; the non-canonical ones do not decode; an honest signature with
   its R replaced by a small-order point is refused). The test fails with
   either check reverted (checked one at a time). The RFC 8032 vectors and
   every caller's tests (`pkg`, `ab`, `update-index`, `x509`, `tls`,
   `ssh`, `xtask` keys) pass unchanged: no honest signature is affected.
2. AES (`libs/crypto/src/aes.rs`): the S-box is the Boyar-Peralta depth-16
   circuit (113 gates, as in BearSSL's `aes_ct`) evaluated on 8 bit planes
   of the 16 state bytes at once, with an 8x8 bit-matrix transpose in and
   out; the key expansion's SubWord runs through the same function. No
   memory address and no branch depends on the key or the data. Test
   `bitsliced_sbox_equals_the_table`: the circuit equals the old table
   (kept in the test, computed as x^254 plus the affine map) for all 256
   inputs in each of the 16 lanes; FIPS 197 appendix C and the GCM vectors
   pass. Speed, host micro-benchmark (`--release`, one core of the
   reference host), table before / bitsliced after:

   | | before | after |
   |---|---|---|
   | AES-128 block | 130-134 ns (119-123 MB/s) | 368-382 ns (42-43 MB/s) |
   | AES-256 block | 176-218 ns (73-91 MB/s) | 514-531 ns (30-31 MB/s) |
   | AES-128 key expansion | 215-221 ns | 552-555 ns |
   | AES-256 key expansion | 273-283 ns | 680-683 ns |
   | AES-128-GCM, 16 KiB records | 43-45 MB/s | 26-27 MB/s |
   | AES-256-GCM, 16 KiB records | 37-40 MB/s | 21 MB/s |

   About three times slower per block and 40 % slower for GCM records
   (GHASH, unchanged, is the rest). The TLS client offers ChaCha20-Poly1305
   first, so AES runs only against servers that insist on it, and HTTPS
   downloads are bounded by the network, not by 20 MB/s.
3. `ct_eq` (`libs/crypto/src/lib.rs`) passes the accumulator through
   `core::hint::black_box` at every byte: the optimizer has to treat the
   value as unknown after each step and cannot shortcut the loop. The API
   is unchanged.
4. Secret lifetime. `wipe` is generic over plain values (`u8`, `u32`,
   `u64`, `u128`), and `SecretBytes<N>` wipes an array when it goes out of
   scope, on every way out of a function (an early `?` included).
   - Drop impls that wipe: `Aes` (round keys; the key expansion's words are
     wiped before `new` returns), `AesGcm` (H), `Poly1305`, `ChaChaPoly`
     (SSH packet keys), `chacha20::Rng` (key and unread output),
     `Sha256` and `Sha512`/`Sha384` (state and buffer; `finish` wipes its
     last block), TLS `Secret` (every HKDF secret of the schedule),
     `Protection` (ChaCha20 key and IV), `Client` (the ephemeral key), SSH
     `Server` (the connection's copy of the host key).
   - Wiped where they end: HMAC's padded key and inner digest, HKDF's last
     `T(i)`, the ChaCha20-Poly1305 and SSH Poly1305 one-time keys and the
     keystream blocks they come from, `xor_keystream`'s last block, the
     generator's reseed mix, Ed25519's expanded key, scalar, nonce hash and
     nonce and the limbs of scalar arithmetic, X25519's clamped scalar and
     its four ladder registers, the TLS ephemeral key right after the
     ServerHello (whatever the outcome), the TLS shared secret and record
     keys, SSH's ephemeral key, K, the exchange-hash input that holds K,
     the derivation buffers and derived keys, the HTTPS client's seed,
     sshd's host key text and hex, `random_seed` text and hex, the kernel
     randomness it mixes and its file read buffer. sshd now hands each
     connection its host key and generator seed by reference, so the
     `Server` holds the only copy and wipes it.
   - Tests: one per type that a drop wipes it (`*_wiped_when_dropped` in
     `aes`, `aead`, `poly1305`, `chacha20`, `ssh_chachapoly`, `sha256`,
     `sha512`, `lib`, `tls::keys`, `tls::record`, `ssh`), and
     `rfc8448_handshake_data_and_close` checks that the TLS ephemeral key
     is zero once connected.

## Not covered

What the fix does not claim:

- **Copies the compiler makes.** A value returned or moved by value (a
  `[u8; 32]` out of `x25519::shared`, `hkdf::extract_*`, `traffic_key` or
  sshd's `Entropy::derive`, an `Option` around it, a struct moved into a
  field) may leave a copy in a stack slot that nobody wipes, and registers
  spill where the compiler likes. Rust gives no way to wipe those from
  safe code; the wipes cover the named buffers only.
- **Working state of the primitives.** The ChaCha20 block function's
  state, the SHA-256/512 message schedule in `compress`, Poly1305's
  locals, the field elements and points inside the Ed25519 signing ladder
  and the per-step temporaries of the X25519 ladder are not wiped:
  wiping each block's state would cost a large share of their speed, and
  the next call overwrites the same stack.
- **Heap copies.** A `Vec` that grows moves its content and frees the old
  buffer without wiping it: sshd's `read_file` (host key text,
  `random_seed`), the SSH `Writer`s that hold K. The final buffer is
  wiped. Freed process memory reaches another process only through the
  kernel, which zeroes pages on first touch (ADR 0049).
- **Secrets that live as long as their program.** sshd's host key seed in
  `main` and its `Entropy` state, the kernel generator (a `static`): they
  are never dropped.
- **Host tools.** `xtask` reads the release signing key on the build host
  (`keys.rs`, `image.rs`); that process is not wiped.
- **Mixed-order Ed25519 keys.** A key with a small-order component added
  to a point of order L is not refused (that needs [L]A, a second scalar
  multiplication). Verification stays cofactorless, so such a key only
  verifies what its holder signed, as RFC 8032 allows.
- **No timing measurement.** AES and `ct_eq` are constant time by
  construction and by reading the source; no dudect-style statistical
  test was run, and `black_box` is documented by Rust as best effort.

## Tests run

- `cargo xtask check`: green (fmt, clippy `-D warnings` on every package,
  stack budgets: 136 binaries, none over).
- `cargo xtask test --level unit`: 709 passed, 0 failed.
- `cargo xtask test --all --area tls,ssh,crypto,update` (at `cfaec96`):
  51 QEMU scenarios, 0 failed (`net.https` with a real TLS 1.3 handshake,
  the six `ssh.*` with key exchange and public-key login, the `update.*`
  and `apps.*` signature checks, `crypto.*`); host temperature at most
  74 °C, the thermal guard paused nothing.

## Close

Cause, fix and evidence above; the four infos of the audit are fixed on
`fix/crypto-hardening`. ADR 0026 (TLS) and ADR 0025 (SSH) carry a note.
What remains open is the list under "Not covered".
