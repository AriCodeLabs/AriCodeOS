# ADR 0067: TLS and X.509 through rustls and rustls-webpki

- **Date:** 2026-10-31
- **Status:** Accepted. Step 4 of ADR 0063's order of work. The owner chose
  on 2026-10-31, after the measurements below:
  - to enable TLS 1.2 with rustls's safe subset;
  - to take the trust anchors from Mozilla's store.

  Changes ADR 0026: TLS 1.3 only, 17 embedded roots.

## Context

The TLS 1.3 client (`aricode-tls`) and the X.509 verifier (`aricode-x509`)
were written here (ADR 0026). Their recent fixes, matching by common name
(ADR 0058) and wildcards over public suffixes (ADR 0062), were mistakes
established libraries had already made and fixed. ADR 0063 asks for the
comparison with rustls and rustls-webpki: maintained, audited, used by a
large part of the Rust ecosystem, `no_std` with `alloc`, under Apache-2.0,
ISC or MIT.

## Measurements

### X.509: x509-limbo

[x509-limbo](https://github.com/C2SP/x509-limbo) (C2SP, commit 1080e56)
is the path validation suite webpki, OpenSSL, Go and others are measured
against. Of 9,785 server cases (10 client, 5 chain-depth and 2 key-usage
cases skipped):

| | Correct | Wrongly accepted | Wrongly refused |
|---|---|---|---|
| `aricode-x509` | 9,047 | **297** | 440 |
| `rustls-webpki` 0.103 | 9,717 | 60 | 8 |
| This ADR (webpki and our policy) | 9,714 | 63 | 7 |

- Our verifier ignored name constraints that are not critical. A CA
  constrained to its own domains, such as a government's or a company's,
  could have issued a certificate for any site, and we would have
  accepted it (240 cases).
- It also failed to build paths through cross-signed CAs (21 cases),
  which breaks real servers.
- What `rustls-webpki` still accepts is mostly RFC 5280 strictness it
  leaves out on purpose, plus revocation lists.
- The verifier of this ADR differs from `rustls-webpki` alone in six
  cases:
  - it accepts 4 more, revoked through CRLs: revocation is not checked,
    as before;
  - it refuses a wildcard over a public suffix that `rustls-webpki`
    accepts (ADR 0062);
  - it accepts a name-constraint case that `rustls-webpki` gets wrong.

### TLS: real servers

74 HTTPS hosts: the update source and GitHub's download hosts, large
sites and CDNs, governments and badssl.com's test hosts. Both clients used
the same root store.

| | Both clients | Only rustls | Only ours | Neither |
|---|---|---|---|---|
| `aricode-tls` (before) | 53 | 11 | 0 | 10 |
| This ADR, before requiring EMS | 63 | 1 | 0 | 10 |
| This ADR, extended master secret required | 55 | 8 | 0 | 11 |

- The old client failed with every TLS 1.2-only server:
  - `www.elpais.com` and `www.baidu.com`;
  - all of badssl.com's valid hosts.
- The one left, `azure.microsoft.com`, takes 8 seconds to answer either
  client and fails each in turn.
- Requiring the extended master secret (decision 3) costs the hosts whose
  TLS 1.2 lacks it:
  - all of badssl.com's, deliberately old servers;
  - `1.1.1.1`, which another certificate check refused anyway.

  Every other site of the list keeps working, `www.elpais.com` and
  `www.baidu.com` included.
- Both refuse the expired, wrong-name, self-signed, unknown-root, TLS 1.0
  and 1.1, and RSA-1024 hosts.
- Neither checks revocation (`revoked.badssl.com`).

### Speed

Handshake and one request against a local `openssl s_server`, P-256
certificate, reference host:

| Client | Time |
|---|---|
| `aricode-tls` (before) | 840 µs |
| This ADR (rustls, `aricode-crypto` provider) | 720 µs |
| rustls with `ring` | 410 µs |

The remaining gap is the provider's arithmetic, not the protocol. A
handshake verifies two ECDSA P-256 signatures: 128 µs each in
`aricode-crypto`, about 35 in `ring`'s assembly. `ring` builds C code and
needs `getrandom`, so it cannot run in AriCodeOS as it is.

## Decision

1. **`aricode-tls` drives rustls 0.23 through its unbuffered API**, with
   no `std` and none of rustls's own providers.
   - The interface stays (`Client::start`, `feed`, `send`, `close`,
     `Event`, `Error`), so `aricode-https`, the shell and the updater
     barely change.
   - `start` now returns an error: an invalid server name fails at once.
   - An error from `start` or `send` is reported by the next `feed`.
   - Server bytes held while a handshake message is incomplete are
     limited to `MAX_INCOMING`: the largest message plus one record.
2. **The provider is ours** (`libs/tls/src/provider.rs`):
   - from `aricode-crypto`:
     - hashes and HMAC, and through HMAC the TLS 1.3 HKDF and the TLS 1.2
       PRF;
     - AES-GCM and ChaCha20-Poly1305 records for both versions;
     - X25519;
     - the generator, reseeded with the kernel's randomness at every
       connection;
   - from RustCrypto's `p256` and `p384`: the P-256 and P-384 key
     exchanges. `aricode-crypto` only verifies on those curves, without
     constant time.
3. **Versions and algorithms.**
   - TLS 1.3, and TLS 1.2 with rustls's subset: ECDHE only, AEADs only.
     CBC, RC4, static RSA key exchange, TLS 1.0 and 1.1 are not possible.
   - TLS 1.2 requires the extended master secret (RFC 7627). The owner
     approved TLS 1.2 described as "with extended master secret".
     rustls supports it but does not require it by default. Told of the
     difference and its cost, the owner left the choice to the best for
     speed and security (2026-10-31). It stays required:
     - Speed is the same either way.
     - Without it, a TLS 1.2 session can share its master secret with
       another one (the triple handshake). That needs resumption or
       renegotiation, which this client does not do today. Requiring it
       now means adding resumption later cannot reopen the attack.
     - RFC 9325 (BCP 195) asks TLS 1.2 clients to use it.
     - The measured cost is badssl.com's deliberately old hosts and
       `1.1.1.1`, which another check refused anyway (see "TLS: real
       servers").
   - Key exchange: X25519 first, then P-256 and P-384, so a server
     preferring a NIST curve answers with a HelloRetryRequest, which the
     old client could not follow.
   - Server signatures:
     - ECDSA P-256 and P-384 with SHA-256 or SHA-384;
     - Ed25519;
     - RSA-PSS and PKCS#1 v1.5 with SHA-256, 384 or 512, on moduli of
       2048 to 8192 bits.

     They are verified by `aricode-crypto`. SHA-1 and P-521 are refused.
4. **Chains are verified by `rustls-webpki`, with this system's policy on
   top** (`libs/tls/src/verify.rs`, `aricode-x509`):
   - **Time.** The time a chain is checked at is the clock, or without a
     trusted clock the release's floor. A certificate issued after the
     floor is then checked at its own `notBefore`, so only
     `notAfter >= floor` is required (audit of incident 0036, H1).
   - **Distrusted keys** (ADR 0054). No key of the leaf, an intermediate
     or the anchor may be in `DISTRUSTED`. Mozilla's store holds the emSign
     roots of the compromised eMudhra intermediates, so this list is what
     refuses them.
   - **No wildcard over a public suffix** (ADR 0062).
   - **Names** are matched by `rustls-webpki` on subjectAltName only (ADR
     0058). An IP address matches an iPAddress name only (RFC 6125).

   The reason of a refusal reaches the user as before
   (`aricode_x509::Error`); one is new, `WildcardOverSuffix`.
5. **Trust anchors: Mozilla's TLS server roots.**
   - They are extracted from NSS's `certdata.txt` (MPL-2.0) by
     `tools/mozilla-roots.py` into `libs/x509/roots/mozilla.der`, with
     `mozilla.txt` listing fingerprints and labels for review.
   - Left out, since a plain anchor cannot express what Mozilla applies
     to them:
     - roots with a server distrust-after date (none is trusted for
       servers today);
     - roots with name constraints Mozilla applies in NSS's code
       (CCADB's "Mozilla Applied Constraints"), which `certdata.txt`
       does not carry. The script requires CCADB's report.

     Today that excludes "TUBITAK Kamu SM SSL Kok Sertifikasi - Surum 1",
     limited by Mozilla to `*.tr`.
   - That gives 117 roots, replacing the 17. They are parsed once per
     program (`aricode_tls::mozilla_roots`), and the anchors borrow them
     where the binary holds them.
6. **`aricode-x509` keeps only the policy**: the errors, the time rules,
   the distrusted keys, the public suffixes and the roots. Its parser,
   chain builder and name matching are deleted, and so is the old TLS
   state machine.
7. **Supply chain** (ADR 0063, point 4).
   - User programs gain:
     - `rustls`, `rustls-webpki`, `rustls-pki-types`, `untrusted`,
       `once_cell`;
     - the RustCrypto stack for P-256/P-384: `p256`, `p384`,
       `elliptic-curve`, `primeorder`, `ff`, `group`, `crypto-bigint`,
       `sec1`, `der`, `base16ct`, `const-oid`, `hkdf`, `hmac`.
   - Build scripts: `rustls` (rustc feature detection). No new procedural
     macro.
   - Host tests add `ring`, a rustls server's signer, and `rcgen`, the
     test certificates' maker, both dev-dependencies only.
   - `cargo metadata` lists `rustls-webpki` as linking `ring` even with
     the feature off. So the check of user programs' dependencies
     (`tools/xtask/src/deps.rs`) now reads the build graph from
     `cargo tree`, which shows what is actually built.

## Consequences

- The shell's `http` and the updater reach TLS 1.2 servers and
  NIST-curve-only servers.
- From the guest, through QEMU's network:
  - the shell fetched `www.aricodeos.org`, `www.google.com`,
    `github.com` and `www.elpais.com` (TLS 1.2);
  - the updater fetched the GitHub release index, following its
    redirects. It refused the index's signature, as a test build must.
- `shell` and `updater` grow from about 520 KB to 1,065 KB each,
  stripped. The system image stays well inside 12 MiB.
- Their deepest stack paths shrink: the old verifier's frames are gone.
- Incident 0054 was found here: prints over 4 KiB were lost.

## Limits

- Revocation (CRL, OCSP) is still not checked.
- ECDSA P-256 verification is the slow part of a handshake; a specialized
  field implementation is future work.
- Session resumption across connections is not kept: each `Client` is one
  connection.

## Audit (2026-10-31)

An independent audit found no high findings and no defect in record
protection or key exchange. It checked the TLS 1.3 and TLS 1.2 nonce and
AAD construction against rustls's own provider, the time policy loop, the
anchor key reconstruction, the name checks and the root extraction. Its
findings and what was done:

- **M1.** Nothing limited the bytes held while a handshake message is
  incomplete. rustls keeps every fragment in the caller's buffer, headers
  and padding included. A server could fill the program's 256 KiB heap
  with tiny fragments before being authenticated, and crash the shell.
  - Fixed: `MAX_INCOMING`.
  - A host test sends a 64 KiB ServerHello in 1-byte fragments and is
    refused before the limit.
- **M2.** Mozilla's applied name constraints were lost: TUBITAK's root
  would have been trusted for every name.
  - Fixed: such roots are left out, using CCADB's report (decision 5).
- **L3.** The RSA key parser underflowed on an empty modulus and took any
  exponent, even e = 1.
  - Fixed: the modulus must not be empty, and the exponent must be odd,
    at least 3 and at most 33 bits.
- **L4.** The distrusted-key check ran after path building, so a
  distrusted path stopped the search instead of letting another path be
  tried. Also, `rustls-webpki`'s own `CertDistrusted` was reported as
  malformed.
  - Fixed: the check now runs as `verify_for_usage`'s path callback, and
    that error maps to `Distrusted`.
- **L5.** Some errors were misreported or lost:
  - a bad handshake signature read as malformed;
  - errors from `start` and `send` were dropped;
  - an invalid host only failed at the reply timeout.

  Fixed (decision 1).
- **L6.** Compressed P-256/P-384 key shares were accepted. Now only
  uncompressed points are.
- **L7.** The extended master secret was not required. It is now
  (decision 3, with its cost measured).
- **L8.** Every connection copied all the roots, about a quarter of the
  heap. They are now parsed once and borrowed.
- **L9.** The dependency check's `cargo tree` ran without the user
  programs' flags. It now uses them.
- **L10** (info). The generator has no fast key erasure between reseeds.
  It is `aricode-crypto`'s, shared with the kernel; left for its own
  change.

## Tests

- **Host, against a rustls server in memory** (certificates made at run
  time by `rcgen`, so no private key is committed):
  - every TLS 1.3 suite and every TLS 1.2 ECDSA suite;
  - X25519, P-256 and P-384, a HelloRetryRequest included;
  - Ed25519 and P-384 certificates, records split into single bytes;
  - an unknown issuer, a wrong name, an expired or not-yet-valid
    certificate, and a wildcard over `co.uk`;
  - the floor without a trusted clock, a tampered record, a server alert;
  - a handshake message in 1-byte fragments, cut at `MAX_INCOMING`;
  - an invalid server name;
  - key share formats, and RSA exponents.
- **Host, against `openssl s_server`:**
  - every TLS 1.3 suite;
  - TLS 1.2 with RSA, and with ECDSA and ChaCha20;
  - a HelloRetryRequest for P-256;
  - RSA, P-384 and Ed25519 certificates;
  - a CertificateRequest;
  - a wrong name, an untrusted CA, and an impostor CA with the same name.
- **`net.https`** fetches over TLS 1.3 and TLS 1.2 from host servers and
  refuses a self-signed one. The test certificate now names `10.0.2.2` as
  an iPAddress.
- **`aricode-x509`:** the Mozilla store splits into exactly the
  certificates `mozilla.txt` lists, in order.
