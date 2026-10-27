# ADR 0058: The subject common name is never a host name

- **Date:** 2026-10-27
- **Status:** Accepted (failure-pattern audit 2026-10-25: D-L1, the part
  of L23 left open; incident 0048). Amends ADR 0026 point 2.

## Context

ADR 0026 matched a server's host name against the certificate's
subjectAltName dNSNames and, when the certificate had none, against the
first commonName of its subject. That fallback is what RFC 6125 6.4.4
allowed for old certificates; RFC 9525 (2023), which replaces it, drops
it, the CA/Browser Forum Baseline Requirements have required a
subjectAltName for every name since 2012, and current clients do not read
the common name for publicly trusted certificates (Chrome since 58, Go's
crypto/x509 since 1.17, rustls-webpki never did).

Keeping it means a second, weaker name check: the common name is free
text that nameConstraints do not cover (ADR 0026 already refused a
constrained chain without a subjectAltName for that reason), that a CA
need not validate as a host name, and that only certificates no public CA
issues any more rely on.

## Decision

1. `aricode_x509::matches_hostname` reads only the subjectAltName: a
   dNSName for a host name; for an IPv4 host, an iPAddress entry with that
   address or a dNSName that spells it exactly (RFC 9525 6.2; IPv6 entries
   are skipped, as this client does not connect by IPv6 address). A
   certificate without a matching entry is refused with
   `Error::NameMismatch`, whether the chain ends at an embedded root, at a
   root the owner added, or at the leaf itself pinned as a root.
2. `Certificate` no longer carries the common name: nothing reads it. The
   subject Name is now checked for its structure (SETs of
   AttributeTypeAndValue) to its end; before, the walk stopped at the
   first common name.
3. Unchanged: one left-most wildcard label, at least two labels after it,
   no wildcard for an IP host, nameConstraints over every dNSName; a
   constrained chain needs a dNSName, since no iPAddress constraint is
   evaluated.

## Consequences

- A server whose certificate names its host only in the common name (a
  self-made certificate on an old device, say) is refused. The owner
  reissues it with a subjectAltName (`-addext subjectAltName=DNS:host` in
  OpenSSL). Every certificate of the test scenarios and every public site
  checked (GitHub's update source, `www.google.com`) carries one.
- The RFC 8448 handshake trace's server certificate names `rsa` only in
  its CN: the TLS unit tests that replay it accept it through a test-only
  switch (`trust_pinned_leaf`, compiled only under `cfg(test)`), and a test
  checks that the real path refuses it.
- Not done: wildcards over public suffixes (`*.co.uk` is accepted, since
  it has two labels after the `*`). Refusing them needs the Public Suffix
  List in the trust base; public CAs are already forbidden to issue such
  names, so it remains an open item of the audit.
