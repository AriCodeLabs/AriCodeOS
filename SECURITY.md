# Security policy

AriCodeOS is an experimental operating system under active development
(version `0.x`). It is not yet suitable for protecting real data, but its
security model (capabilities, isolated user-space drivers, signed images
and updates, closed-by-default services) is taken seriously and reports
are welcome.

## Reporting a vulnerability

Please do **not** open a public issue for a security problem. Report it
privately through GitHub's "Report a vulnerability" form on the
repository's Security tab (private vulnerability reporting), or by
contacting the author through the profile at
https://github.com/lynxcraft.

Include what you can of: the version or commit, the command line used to
boot, how to reproduce it, the serial log and the `diag` output, and what
you think the impact is. You will get an acknowledgement, a tracking
reference and, once fixed, credit in the incident record unless you prefer
not to be named.

## What counts

Anything that breaks the rules the system promises: a process reaching a
resource it holds no handle for, a driver escaping its IOMMU domain, an
unsigned or downgraded image being installed, a service answering on a
port that should be closed, a secret derived from a predictable source, or
a refusal that is not logged.

## Supported versions

Only the latest tagged version on `main` and the current `develop` receive
fixes. A security fix on a published version goes through a `hotfix/*`
branch and a new tag, as [docs/WORKFLOW.md](docs/WORKFLOW.md) describes.

## How fixes are handled

A confirmed report is treated as an incident
([docs/FAILURE-PROTOCOL.md](docs/FAILURE-PROTOCOL.md)): recorded, studied,
fixed with a regression test and audited. The incident record is published
with the fix, with any detail that would help an attack withheld until the
fix is released.
