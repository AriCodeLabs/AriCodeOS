# Incident 0048: A certificate's common name was read as a host name

- Opened: 2026-10-27. Status: fixed on `fix/x509-no-cn-fallback`
  (ADR 0058) and, for wildcards over public suffixes, on
  `fix/x509-public-suffix-wildcards` (ADR 0062).
- Found by: the failure-pattern audit of 2026-10-25 (audit D, L1; the
  part of L23 left open by incident 0029). Not seen on a machine.
- Environment: every HTTPS connection (`aricode-tls` through
  `aricode_x509::verify_chain`): the updater, `fetch`, apps with `net`.

## Evidence

At develop `5ef2f6e`, a host test on `libs/x509` asserting that the test
leaf `ed-leaf` (no subjectAltName, subject `CN=ed.example.net`) does not
match `ed.example.net` failed: `matches_hostname` returned true. The
committed test `a_common_name_is_no_host_name` makes the same check on a
dedicated fixture, `cn-only-leaf`, since `ed-leaf` now carries a
subjectAltName (section "Fix").

## Mechanism

`matches_hostname` fell back to the subject's first commonName when the
certificate had no dNSName (ADR 0026 point 2). The common name is not a
validated host name: nameConstraints do not apply to it, and no public CA
issues certificates that depend on it. The fallback was a second, weaker
path to the same decision.

## Fix

`matches_hostname` reads only dNSNames; `Certificate` drops the
`common_name` field, and the parser keeps checking the subject Name's
structure without reading it (ADR 0058). The test fixtures were
regenerated (`libs/x509/testdata/generate.sh`): `ed-leaf` now carries
`DNS:ed.example.net`, and the new `cn-only-leaf` (`CN=www.example.org`, no
subjectAltName, under the EC test intermediate) is refused with
`NameMismatch` by `verify_chain`. `nc-cn`, the constrained CN-only leaf,
now fails at the name check before the constraint check. In `aricode-tls`,
the RFC 8448 trace's certificate (CN `rsa` only) is accepted in unit tests
through a `cfg(test)` switch, and `tampering_and_untrusted_certificates`
checks that the real path refuses it for `rsa` and for `server`.

## Audit (2026-10-27)

An independent audit of `326d26e` found no high or medium finding (no
common-name path left, Name parsing stricter, not looser; the test switch
cannot reach a non-test build) and four low ones, all followed up:

- The constraint check's "no dNSName" branch had become unreachable from
  `verify_chain` (its only test, `nc-cn`, now stops at the name check): a
  direct test of `check_name_constraints` covers it, with a CN-only leaf
  and with a leaf named only by iPAddress.
- No TLS unit test reaches a successful `verify_chain` any more: the
  OpenSSL integration tests (`libs/tls/tests/openssl.rs`), which do, now
  fail instead of skipping when `openssl` is missing.
- A self-made certificate for a device reached by address, with
  `IP:a.b.c.d` in its subjectAltName and the address in its CN, had worked
  only through the fallback, since iPAddress entries were not read: an
  IPv4 host now matches an iPAddress entry (ADR 0058 point 1).
- This section's evidence named a test that used a fixture absent at
  `5ef2f6e`: reworded.

Also from the audit: the shell names the subjectAltName in its message,
and ADR 0058 no longer claims Firefox ignores the common name in every
case.

## Wildcards over public suffixes (2026-10-27)

Left open by the fix above and closed on
`fix/x509-public-suffix-wildcards` (ADR 0062). Evidence: at develop
`49b3c28`, `name_matches("*.co.uk", "example.co.uk")` returned true (the
wildcard needed only two labels after it); the host test
`no_wildcard_over_a_public_suffix` fails with the suffix check disabled
(`*.co.uk matched example.co.uk`). The base of a `*.` name is now checked
against the ICANN section of the Public Suffix List, shipped as an
MPL-2.0 extract with the owner's approval; the private section
(`github.io`) is not used.
