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

## Resolution (commit 8886d1e)

| Id | Resolution |
|----|------------|
| H1 | Unknown installed system version refused without `--allow-downgrade`, fault `upd-installed-unknown`: `apps/updater/src/main.rs` `decide` (500-513), used by `report`, `upgrade` and `apply`. |
| H2 | `update` writes the plan (`report`, main.rs 847-864), `upgrade` installs only planned items (`upd-not-reported`, 1132); packages staged as `updater/pkg-N.aripkg` + `updater/staged` (`stage_app` 1011); installer `STAGED` mode only (`apps/installer/src/main.rs` `install_staged` 327, stage-file names only 337, list emptied 380), `ALLOW_DOWNGRADE` only with `STAGED` (424); lists in `libs/update-index/src/lists.rs`. |
| H3 | `publish-update` writes `manifest.txt` by default (`tools/xtask/src/image.rs` 1131), `--no-legacy-manifest` (`main.rs` 205); `docs/hardware.md`, ADR 0029 decision 9. |
| M1 | Fault events: `Disk::open` (updater 314-361), key (1225), services (1243), invalid `update_url` (229, shell 956), list changed (801), no verified index (793), installed list missing (255); installer: invalid installed package listed and loudly replaced (177, 232). |
| M2 | Updater caps `system, fs, net, block` (`apps/updater/updater.manifest`), removed from `TRUSTED_INSTALL` (`libs/manifest/src/lib.rs` 62); installed apps via installer `LIST` (`write_installed` 187, shell `list_installed` 813); raw `block` limit recorded as follow-up in ADR 0029 decision 8. |
| M3 | Bounded sink and run deadline: updater `fetch` (591), `RUN_DEADLINE_MS` (62), fault `upd-deadline`. |
| M4 | `apply` under the downgrade rule (543), shell `update apply --allow-downgrade` (798). |
| M5 | Disk opened only when a system image is involved (`report`, `upgrade`). |
| M6 | Reasoning in `apps/installer/src/main.rs` 23-27 and ADR 0029: the 64 KiB quota is DMA only; packages use the 256 KiB static heap, peak under 80 KiB. Not tight. |
| L1 | Old header zeroed and flushed first (`Disk::install`, 424-440). |
| L2 | Built in `NAME.new`, moved into place (`image.rs` 1054-1060); signing key below. |
| L3 | `index_serial` (image.rs 1000); recovery documented (ADR decision 6) and `forget_stored` (updater 666). |
| L4 | `normalize_source` (`libs/update-index/src/sources.rs` 48-80). |
| L5 | `MAX_APP_NAME = 20` (`libs/update-index/src/index.rs` 14), checked at publish (parse-back). |
| L6 | Documented in ADR 0029 (limits): four stage slots, at most 16 files in `updater/`. |
| L7 | Open, listed in ADR 0029. |
| Key | `aricode_crypto::trust` (`libs/crypto/src/trust.rs` 13) used by AriBoot, init, installer, updater; `--signing-key DIR` (`tools/xtask/src/keys.rs` 30, refuses in-repo seeds 50, warning 35; `build.rs` 81). |

New scenarios (not run): `update.tampered_image`, `update.stale_index`,
`apps.downgrade_refused`.

## Re-audit (2026-10-17)

Verdict: safe to merge after QEMU, provided R1 is fixed. All items below
are fixed on the branch (commit after this section's first version; line
numbers as of that commit).

| Id | Finding | Resolution |
|----|---------|------------|
| R1 | The build could silently embed the dev key: `signing_public_hex` reread `DIR/ed25519.pub` on every `cargo()` call and a read error meant "dev key". | The verified public key (bytes and hex) is stored with the directory in a `OnceLock` at selection and never reread (`tools/xtask/src/keys.rs` 31, 96, 102); `cargo()` uses it (`build.rs` 80). Post-build check: every verifier binary must carry the trust anchor with exactly the expected key (`keys.rs` 112, called from `image.rs` 33-35), and every valid system image (`image.rs` 557) and package (`build.rs` 324) must verify with it; otherwise the build fails. Checked by hand with a production key and with the dev key. |
| R2 | Invalid `ARICODE_SIGNING_PUB` only failed at run time; stale AriBoot message. | The key is decoded by a `const fn` into the tagged `TRUST_ANCHOR` static (`libs/crypto/src/trust.rs` 33, 53): an invalid key is a compile error (checked: `ARICODE_SIGNING_PUB=zz` stops the build with "expected 64 hex digits"); `public_key()` (77) reads it with a volatile load so the bytes stay in the binary. AriBoot's "invalid development public key" branch is gone (`boot/src/slots.rs` 91), as are the run-time key checks in init, the installer and the updater. |
| R3 | The 32-entry AriFS table could fill and block `update`. | `installed`, `plan` and `staged` are sections of one `updater/state` file (`libs/update-index/src/lib.rs` 84, `lists.rs` 31-48; updater 248-256, installer 186-193); the four stored indexes are one `updater/indexes` file (`lib.rs` 87, `stored.rs` 73-86, updater 690). A first `update` plus a system `upgrade` need four entries; disk variant `fs-nearly-full` (`image.rs` 718-722) and scenario `update.fs_nearly_full`. |
| R4 | Legacy `manifest.txt` could downgrade ADR 0027 machines. | `publish-update` refuses to write it for a version below the published one (`image.rs` 1119, host test); ADR 0029 decision 9 documents making it opt-in after the transition release. |
| UX | Upgrade was silent when the plan had no system line but an index offered a newer system. | `upd-not-reported` fault and message (updater 1163). |

New scenarios (not run): `update.installed_unknown` (H1, disk
`a-corrupt`), `update.apply_refuses_older` (M4, disk
`pending-b-older-update`), `apps.downgrade_allowed` (positive
`upgrade --allow-downgrade` through STAGED), `apps.staged_tampered`
(`inst-stage-hash`, `inst-stage-path`), `update.fs_nearly_full` (R3).

### Final re-audit (2026-10-17)

Verdict: safe to merge once QEMU passes. Three low items, fixed:

| Id | Finding | Resolution |
|----|---------|------------|
| L-a | Read errors silently reset state: `read_stored` and `read_state` treated an unreadable or oversized file as empty, and the next write dropped its content (stored serials included). | Updater: `read_state` (`apps/updater/src/main.rs` 253) returns an error after fault `upd-state-unreadable`, and `write_state_section` refuses to rewrite it; `read_stored` (709) records `upd-indexes-unreadable` and sets `INDEXES_UNREADABLE` (706), after which `write_stored` refuses to rewrite `indexes` for the rest of the run (735). Installer: `read_state` (`apps/installer/src/main.rs` 189) distinguishes a missing file from an unreadable one (`inst-state-unreadable`), `write_state_section` (207) refuses, and STAGED stops without touching the list (359). |
| L-b | The UX branch opened the disk and recorded faults on machines without A/B partitions. | `Disk::probe` (updater 351, over `open_with(false)`, 357) returns `None` silently; the branch uses it (1210). |
| L-c | `update.fs_nearly_full` did not cover an app upgrade. | Five free entries (`FS_FREE_ENTRIES`, `tools/xtask/src/image.rs` 719); the scenario installs hello_ext v1 and upgrades the system and hello_ext 2 through one stage file, ending with `32 file(s)`. |

### QEMU follow-up (2026-10-17)

`update.fs_nearly_full` failed in the first QEMU run (`updater: cannot
write pkg-1.aripkg`, exit 45). Evidence from its `ls`: the updater had
created exactly the four expected files (`state`, `indexes`, `last.log`,
`history.log`), plus `nuevo.txt`, which init's `fs_client` self-test
creates on every boot (`user/testprogs/src/bin/fs_client.rs`). The
updater's count (five) was right; the disk variant forgot the boot-time
entry. Fixed in the variant (`BOOT_CREATED_FILES`,
`tools/xtask/src/image.rs` 733-744). Decision: `upgrade` now decides
everything first and reserves every AriFS entry it will need before the
first irreversible step (`reserve`, `apps/updater/src/main.rs` 192 and
1297-1310, fault `upd-fs-full`), so a full table refuses the whole upgrade
instead of scheduling the system and failing to stage an app; covered by
the new scenario `update.fs_one_short`.
