# Incident 0026: the broker and the installer disagree on how large a package can be

- Opened: 2026-10-24. Status: fixed on `fix/package-size-limit`
  (2026-10-24); larger packages need larger AriFS files (see Open).
- Found by: the debug-build reproduction of incident 0021 (`cargo xtask
  test --debug --all --area update` on `fix/update-image-size`), after the
  system image cases passed: the 7 scenarios still failing all involve an
  app package (`apps.downgrade_allowed`, `apps.downgrade_refused`,
  `apps.update`, `apps.update_cut`, `apps.upgrade_network`,
  `update.fs_nearly_full`, `update.fs_one_short`).
- Environment: any package over 32 KiB. The debug `hello_ext` package is
  75,408 bytes; the release one 14,480.

## What was seen

```text
AriInit: launch hello_ext from AriFS (apps/hello_ext.aripkg version 1, 75408 bytes) with manifest [system fs] memory 64 KiB
hello_ext v1: hello from outside the tree (arg 1)
run: hello_ext exited with 0
installer: apps/hello_ext.aripkg fails verification (it cannot be launched); not listed
updater: app hello_ext: version 2 available (not installed)
```

The broker verified and launched the installed package; seconds later the
installer called the same file unverifiable and left it out of the
installed list, so the updater treated the app as not installed.

## Mechanism

Three limits for one thing: the broker stages a package in a 256 KiB
buffer (`STAGE_LEN`, services/init), while the installer and the updater
read at most `MAX_PACKAGE` = 32 KiB (apps/installer, apps/updater; the
"32 KiB packages" of ADR 0029's limits). The installer maps "larger than
`MAX_PACKAGE`" to `Installed::Invalid`, reported as "fails verification
(it cannot be launched)", which is false for anything between 32 and
256 KiB. The same shape as incident 0021: a size limit reported as a
verification failure.

## Consequences

- A package between 32 and 256 KiB installs and runs but is invisible to
  `update`/`upgrade`: never upgraded, and the message points at its
  integrity instead of its size.
- Debug builds cannot run the app update scenarios.

## Why 32 KiB and not 256

Two facts fix the size. An AriFS file has a fixed capacity of 64 sectors
(32 KiB) and an atomic replace goes through the journal, which holds one
file's capacity (`MAX_REPLACE`): on a machine nothing larger can be
installed or upgraded; only the host's image builder creates larger files.
And the installer and the updater hold packages on the static 256 KiB
heap, the installer two at once (the new one and the installed one), so
256 KiB packages could not fit there anyway. The broker's 256 KiB was the
odd one out: it launched packages the system could never manage.

## Fix

- `aricode_pkg::PACKAGE_MAX` = `aricode_proto::fs::MAX_REPLACE`: the
  broker's staging buffer, the installer's and the updater's limits and
  xtask's package build use it, so they cannot drift apart.
- A package over it is said to be too large, with both sizes, everywhere:
  the broker (`AriInit: refused, package ... is N bytes; packages hold 1
  to 32768`), the installer (installed, in `pkgs/` or staged:
  `inst-installed-size`, `inst-pkg-size`, `inst-stage-size`), the updater
  (`upd-pkg-size` with the size). "Fails verification" now means only
  that.
- xtask refuses to build a package over the limit (`app-pack`,
  `publish-update`, scenario installs), so the debug app scenarios now
  stop at build time saying why instead of failing on the machine.
- Tests: `apps.oversized_package` (a 33,792-byte `apps/big.aripkg` put on
  the disk by the host: refused by the broker, reported as too large by
  the installer); host test of the build guard.

## Open

- Apps larger than 32 KiB (any real one in a debug build; a release app
  with TLS would pass it too) need AriFS files larger than one journal,
  either a larger journal and file capacity or an install that is atomic
  across several journal transactions (ADR 0018). That is AriFS design
  work for the owner's roadmap, not part of this fix.
