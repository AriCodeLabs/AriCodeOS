# Incident 0048: A certificate's common name was read as a host name

- Opened: 2026-10-27. Status: fixed on `fix/x509-no-cn-fallback`
  (ADR 0058).
- Found by: the failure-pattern audit of 2026-10-25 (audit D, L1; the
  part of L23 left open by incident 0029). Not seen on a machine.
- Environment: every HTTPS connection (`aricode-tls` through
  `aricode_x509::verify_chain`): the updater, `fetch`, apps with `net`.

## Evidence

Host test `a_common_name_is_no_host_name` on `libs/x509` at develop
`5ef2f6e`: the test leaf `ed-leaf`, which has no subjectAltName and the
subject `CN=ed.example.net`, matched the host `ed.example.net`
(`matches_hostname` returned true; the test failed).

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

## Left open

Wildcards over public suffixes (`*.co.uk`): ADR 0058, "Consequences".
