# ADR 0062: No wildcard over a public suffix

- **Date:** 2026-10-27
- **Status:** Accepted (failure-pattern audit 2026-10-25: D-L1, the last
  part of L23; incident 0048). The owner approved shipping an extract of
  the Public Suffix List under its own licence, MPL-2.0 (2026-10-27).

## Context

A certificate name `*.example.com` stands for one label under
`example.com`. Since ADR 0026 a wildcard needs at least two labels after
it, so `*.com` matches nothing. `*.co.uk` still matched, and it would
stand for every company registered under `co.uk`: a public suffix, where
names are registered by unrelated owners. The Baseline Requirements
forbid public CAs to issue such a wildcard, but nothing in the client
checked it, and roots an owner adds are not bound by the Baseline
Requirements at all. Browsers refuse it with the Public Suffix List
(Chrome checks a wildcard's base against the list's ICANN section).

## Decision

1. `matches_hostname` refuses a `*.` dNSName whose base (what follows
   `*.`) is a public suffix of the ICANN section of the Public Suffix
   List: a rule names it, or a wildcard rule names its parent and no
   exception rule names it (`*.ck` makes `foo.ck` a suffix, `!www.ck`
   keeps `www.ck` registrable).
2. The private section (`github.io`, cloud and hosting names) is not used:
   those suffixes are run by one operator that holds certificates for
   them (GitHub Pages serves `*.github.io`), and Chrome leaves it out for
   certificates too.
3. Only the rules that can name a wildcard's base are shipped: ICANN
   rules of two labels or more, and every wildcard and exception rule,
   with labels in punycode as dNSNames carry them (5509 rules, 68 KB of
   text, in every binary that verifies certificates: the shell, the
   updater, apps using `sdk/https`). Single-label suffixes are already
   refused by the two-label rule. `libs/x509/psl/update.sh` makes
   `psl/icann-wildcard-bases.dat` from the published list, recording its
   version and SHA-256 in the file's header; nothing is fetched at run
   time.
4. The data file stays under MPL-2.0, with its licence header, and is
   named in `NOTICE`; the rest of the project stays Apache-2.0. MPL-2.0
   applies file by file and is compatible with a larger work under other
   terms.
5. The extract is refreshed like the trust store: the release checklist
   (`docs/WORKFLOW.md`) runs `update.sh` and commits the result when the
   list changed.

## Consequences

- A certificate for `*.co.uk` (or `*.com.au`, `*.foo.ck`) matches no host,
  whoever issued it. `*.example.co.uk` and `*.github.io` match as before.
- A suffix added to the list after a release is known from the next
  release; a name removed from the list stays refused until then. Both
  err towards refusing, and only for wildcard names.
- The lookup is a linear scan of the extract, done only when a wildcard
  name is matched (once per certificate name), not per packet.
