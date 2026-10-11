# ADR 0026: HTTPS with a TLS 1.3 client and X.509 verification

- **Date:** 2026-10-11
- **Status:** Accepted

## Context

`http https://...` on the laptop answered `invalid url`: the system only
spoke plain HTTP. Almost every site needs HTTPS, and HTTPS without verifying
the server certificate would be theatre: anyone on the path could answer.
SSH (ADR 0025) had already brought X25519, ChaCha20-Poly1305 pieces and
SHA-256.

## Decision

1. **Cryptography** (`aricode-crypto`, no alloc): SHA-384, HMAC, HKDF
   (RFC 5869), the ChaCha20-Poly1305 (RFC 8439) and AES-128/256-GCM AEADs,
   big integers and RSA verification (PKCS#1 v1.5 and PSS, up to 4096-bit
   moduli), ECDSA verification on P-256 and P-384 (Montgomery arithmetic,
   Jacobian points). Official vectors plus vectors generated with the
   OpenSSL command line, and negative tests.
2. **X.509** (`aricode-x509`): strict DER, certificate parsing (RSA, P-256,
   P-384 and Ed25519 keys; SHA-256/384/512 RSA, RSA-PSS and ECDSA
   signatures; basicConstraints, keyUsage, SAN), chain building to a trust
   anchor with CA, path-length and date checks, host name matching (SAN
   dNSName with one left-most wildcard, CN only without SAN). 17 embedded
   roots (Let's Encrypt, Google Trust Services, DigiCert, Amazon, Sectigo
   USERTrust, GlobalSign), listed with fingerprints in `libs/x509/roots/`.
3. **TLS 1.3 client** (`aricode-tls`): a no-I/O state machine like
   `aricode-ssh`: x25519 key share, TLS_CHACHA20_POLY1305_SHA256,
   TLS_AES_128_GCM_SHA256 and TLS_AES_256_GCM_SHA384, ECDSA, RSA-PSS and
   Ed25519 CertificateVerify, middlebox-compatible CCS, KeyUpdate,
   NewSessionTicket ignored. Tested byte for byte against the RFC 8448
   trace and against `openssl s_server` (every suite and key type, wrong
   host, unknown CA), and manually against real sites.
4. **Real-time clock**: the kernel reads the CMOS RTC at boot
   (`sysinfo::WALLCLOCK`, `sys::time::unix_seconds`); certificates are
   checked against that date. Without a valid RTC reading, dates are not
   checked.
5. **`aricode-https`** (SDK, needs alloc): HTTP or HTTPS GET over the TCP
   service; `aricode-sys::http::get` stays alloc-free and plain-HTTP only.
   The shell's `http` uses it and adds `trusted_ca.der` from AriFS, if
   present, as an extra trust anchor (private CAs, tests).
6. **Test**: `net.https` starts two `openssl s_server -www` on the host (a
   certificate from a scenario CA installed as `trusted_ca.der`, and a
   self-signed one): the first fetch succeeds, the second is refused with
   `certificate signed by an unknown authority`.

## Certificate purpose (audit of 8910d1f, finding 1)

`verify_chain` rejects a leaf whose `extendedKeyUsage` lacks `serverAuth`
(or `anyExtendedKeyUsage`), or whose `keyUsage` lacks `digitalSignature`,
with `Error::WrongPurpose` (RFC 5280 4.2.1.12; RFC 8446 4.4.2.2, the server
signs with its key in TLS 1.3). A certificate without those extensions is
unrestricted. Test chains `purpose-ok`, `purpose-client` and
`purpose-nosign` cover the control and both refusals; the shell reports
`certificate not issued for a TLS server`.

## Consequences

- No revocation checking (CRL/OCSP), name constraints, IP-address SANs, session resumption, 0-RTT, ALPN or client certificates.
  A HelloRetryRequest (server insisting on another group) fails.
- Only TLS 1.3: servers limited to TLS 1.2 cannot be reached.
- Responses are limited by the shell's 8 KiB buffer; HTTP/1.1 only.
- AES uses table lookups (not cache-timing hardened); public-key
  operations are verification only.
