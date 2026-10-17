# Audit 1 of `feature/update-sources` (ADR 0029)

- **Date:** 2026-10-17
- **Scope:** commits `fbed6e2..c67b8de` (libs/update-index, updater,
  installer, shell, xtask, scenarios, ADR 0029)
- **Verdict:** not mergeable yet; the findings below are applied on the same
  branch (resolution at the end).

## Summary

Trust chain sound: signature before parsing, strict parser, stale serials
refused, verified copies kept, image and packages verified before any
write.

| Id | Severity | Finding |
|----|----------|---------|
| H1 | high | System downgrade when the installed version is unknown. |
| H2 | high | `--allow-downgrade` reaches every package in `pkgs/` (no delete in AriFS). |
| H3 | high | The laptop cannot fetch the new format (no `manifest.txt`). |
| M1 | medium | Refusals without fault events. |
| M2 | medium | `install` grant not least privilege (`block` already reaches the disk, but `install` gives journaled writes to `authorized_keys`, the host key, `sources`, `trusted_ca.der`, `apps/` inside the network-parsing process). |
| M3 | medium | Unbounded index/signature fetch. |
| M4 | medium | `update apply` skips the downgrade rule. |
| M5 | medium | Disk required for apps-only work. |
| M6 | medium | Installer quota unverified. |
| L1 | low | Header-last does not invalidate the old header. |
| L2 | low | Non-atomic publish, always the dev key. |
| L3 | low | Serial from the clock. |
| L4 | low | Normalization gaps. |
| L5 | low | App name length. |
| L6 | low | AriFS 32-entry table. |
| L7 | low | Index not bound to its source. |

Dev key committed: anyone on the LAN can sign images and packages on
dev-key builds; a production key is required before network upgrades on
untrusted networks.

## Resolution

See the commit after this file on the branch and the report attached to
it: every item is either fixed (with the file and line) or recorded as
open in ADR 0029.
