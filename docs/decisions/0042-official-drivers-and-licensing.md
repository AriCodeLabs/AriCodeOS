# ADR 0042: Official drivers, protocol stability and licensing

- **Date:** 2026-10-23
- **Status:** Proposed (draft, recorded so the question is not lost). The
  licence choice belongs to the owner; nothing changes until he decides.

## Context

A question raised by the owner: Linux's monolithic kernel is often said to
have prevented fragmentation, because every vendor's drivers and protocols
end up in one centrally reviewed tree. AriCodeOS is a microkernel: drivers
are ordinary processes that talk to the rest of the system through stable
protocols (`sdk/proto`, ADRs 0023, 0024, 0031, 0036, 0037). That is what
makes them isolated, restartable and least-privileged (ADR 0005, 0016,
0038), but it also means a third party could ship a closed driver that
speaks the protocol without contributing anything back.

Linux's cohesion comes mostly from governance rather than architecture:
one tree with maintainers, no stable in-kernel driver interface (out-of-tree
drivers are costly to keep), and the GPL, which requires the source of
distributed modifications. A monolithic kernel without those rules still
fragments: Android's vendor kernels did, and stable interfaces had to be
added later to contain it. Microkernels are not the cause of fragmentation
either (QNX, seL4, Fuchsia).

AriCodeOS is licensed under Apache-2.0 (with NOTICE and the attribution
requirement). Apache-2.0 requires attribution and the licence notice, but
not the publication of modified sources or of separately written drivers.

## Options to contain fragmentation (proposed)

1. **One official repository with the drivers inside it.** Official drivers
   live in `services/` of `AriCodeLabs/AriCodeOS`, reviewed under
   CONTRIBUTING.md and the failure protocol, with tests in the repository.
2. **Protocols as versioned contracts.** Every driver-facing protocol has a
   version and an ADR; additive changes keep the version, incompatible ones
   bump it, and the system refuses mismatched peers visibly (already the
   practice for the ABI and `sdk/proto`).
3. **Official means signed.** The system image and packages signed with
   the release key (ADR 0029, `keys/release/`) are the official build; a
   driver that does not come from the project can be installed only as an
   external package and is shown as not official in `diag`/`services`
   (to be designed: a field in the manifest and the package signature).
4. **A written acceptance policy for drivers**: what a contributed driver
   must bring (tests, scenario, capability manifest, audit for DMA/IRQ
   paths) before it becomes official.
5. **Licence (owner's decision).**
   - Keep **Apache-2.0**: easiest adoption, attribution protected, but
     closed forks and closed drivers are allowed.
   - Move to **GPL-2.0 or GPL-3.0**: distributed modifications must publish
     their source, which restrains closed forks; less attractive to some
     companies; all past contributions are the owner's or AI-assisted under
     his direction (AUTHORS.md), so relicensing is possible now and harder
     later.
   - **MPL-2.0** (file-level copyleft): modified files must stay open,
     separate new files (e.g. a new driver) may be closed; a middle point.
   - Any change of licence updates LICENSE, NOTICE, the SPDX headers,
     AUTHORS.md, AI-NOTICE.md, llms.txt and the READMEs together.

## Recommendation (to be confirmed)

Adopt 1 to 4 regardless of the licence: they are governance and cost
little. Decide 5 deliberately, and early if a copyleft licence is wanted,
because relicensing gets harder once outside contributors join.

## Consequences

- Nothing changes in the code or the licence now.
- If adopted, option 3 needs an ADR of its own for the "official" flag in
  manifests and packages, and option 4 becomes a section of CONTRIBUTING.md.
