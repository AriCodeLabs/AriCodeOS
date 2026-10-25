# Incident 0029: restrictions on intermediate certificates were ignored

- Opened: 2026-10-25. Status: fixed on `fix/x509-intermediate-constraints`.
- Found by: the failure-pattern audit of 2026-10-25 (audit D, H1 and H2:
  the webpki/JSSE pattern, a restriction set on a sub-CA silently
  dropped). Not seen on a machine; the embedded roots' real chains carry
  neither restriction in a way that changes their result.
- Environment: every HTTPS client of the system (shell `http`, the
  updater's downloads, `aricode-https`).

## Evidence

The verifier of `develop` at `0fd600a`, with the test chains of this
branch (`libs/x509/testdata/generate.sh`):

```text
old code accepts: nc-evil true, eku-mail true
```

- `nc-evil`: an intermediate whose (non-critical) nameConstraints permit
  only `good.example` issued a certificate for `www.evil.example`;
  `verify_chain` accepted it.
- `eku-mail`: an intermediate whose extendedKeyUsage is e-mail protection
  only issued a TLS server certificate; `verify_chain` accepted it.

## Mechanism

`Parsed::parse_extensions` handled the extensions it knew and, for the
rest, refused critical ones and skipped the others. nameConstraints was
not among the known ones: a CA that marked it critical made its whole
chain unusable, and one that did not (as the CA/Browser Forum allows) had
its constraints dropped. extendedKeyUsage was parsed for every
certificate but checked only on the leaf, so a CA limited to other
purposes could still vouch for a TLS server. Both are the same shape:
a restriction parsed (or not) in one place and enforced in none along
the path.

## Fix

- nameConstraints are parsed (dNSName subtrees) and applied to every
  name of the leaf by each constrained CA of the chain, root included;
  extendedKeyUsage of every CA must allow serverAuth (ADR 0026,
  "Constraints on intermediates").
- In the same branch, the audit's low findings on the same parser: two
  instances of one extension, a CA certificate as the server's, empty or
  control-byte dNSNames, and RSA keys whose size or exponent makes a
  verification costly (B, L4: an exponent of 120 000 bytes cost 11 s).
- Host tests `name_constraints_bind_intermediates`,
  `intermediates_need_server_auth`, `name_constraint_subtrees`,
  `hardened_parsing`. The first two failed with the old verifier, as
  above.
