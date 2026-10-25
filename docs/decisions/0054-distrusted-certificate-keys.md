# ADR 0054: Distrusted certificate keys, refused anywhere in a chain

- **Date:** 2026-10-25
- **Status:** Accepted (failure-pattern audit 2026-10-25: D-M3)

## Context

The trust store is `aricode_x509::roots::DEFAULT`, a list of root
certificates. The only way to stop trusting something was to drop a root
from it. That does not cover the October 2026 case behind the
`webpki-roots` advisory: an emSign intermediate that should not be
trusted, issued under a root that stays trusted. Nor does it cover a root
whose key is also cross-signed by another trusted root: removing the root
certificate leaves the cross-signed intermediate, with the same key,
valid.

## Decision

1. `aricode_x509::roots::DISTRUSTED` lists the SHA-256 digests of the
   SubjectPublicKeyInfo of keys no longer trusted. A key is named, not a
   certificate: every certificate for that key, reissued or cross-signed,
   is covered.
2. `verify_chain` refuses (`Error::Distrusted`) a chain in which the leaf,
   any intermediate on the path, or the root it ends at has a listed key,
   whatever signed it. `verify_chain_with` takes the list explicitly
   (tests).
3. The list ships with the system and changes with a release: the release
   checklist (`docs/WORKFLOW.md`) checks the week's certificate advisories,
   and each entry carries a comment naming the certificate and the
   advisory. Updates are signed and installed like any other change, so
   the list never comes from the network on its own.
4. The list is empty today: none of the embedded roots, and no
   certificate this system is known to meet, is affected by a current
   advisory (the emSign roots are not embedded).

## Consequences

- A distrust takes effect with the release that carries it, not before:
  a machine that does not update keeps trusting the key.
- The update signing keys are out of scope: their rotation and
  revocation is ADR 0041, whose timing is the owner's decision.
