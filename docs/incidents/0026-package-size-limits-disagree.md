# Incident 0026: the broker and the installer disagree on how large a package can be

- Opened: 2026-10-24. Status: open, documented only.
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

## Open

- One package limit shared by broker, installer and updater (a constant
  in a shared library, as `IMAGE_DOWNLOAD_MAX`), sized against the
  installer's and updater's memory, and a distinct "package larger than
  N" status and message.
- A guard in `app-pack` / `publish-update` for packages over that limit.
