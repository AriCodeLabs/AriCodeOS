# Incident 0052: upgrade after a new release failed with a bare "HTTP 404"

- Opened: 2026-10-28. Status: fixed on `fix/upgrade-after-new-release`.
- Found by: the reference laptop, moving to stable 0.16.0 from GitHub on
  2026-10-27, right after the release was published.
- Environment: a stable source at GitHub's public path
  `https://github.com/AriCodeLabs/AriCodeOS/releases/latest/download/`
  (ADR 0048), registered by the release stick.

## Evidence

On the laptop, `upgrade --channel stable` printed:

```
updater: download failed: HTTP 404
update: updater exited with 41
```

Nothing was written. A new `update` then reported system 16, and
`upgrade` installed it. Reproduced in QEMU with the new scenario
`update.withdrawn_image` before the fix (log
`/home/serverbig/aricode-logs/0052/repro-serial.log`): the same two
lines.

## Mechanism

`upgrade` installs only what the last `update` reported, from the index
that `update` verified and stored (ADR 0029, audit 1 H2): the owner sees
the plan before anything is installed. The stored index names its files
relative to the source URL. The source here is GitHub's `latest/download/`,
which follows the newest release. The laptop's last `update` ran before
0.16.0 was published, so its stored index named `aricodeos-15.img`. Once
0.16.0 was out, `latest/download/aricodeos-15.img` pointed into the
0.16.0 release, which does not carry that file, and GitHub answered 404.

ADR 0048 had accepted this for the short window between fetching the
index and fetching the image within one run. It did not consider the
much longer window between `update` and `upgrade`, which can be days.
The behaviour was right: the run failed closed and installed nothing.
The defect was the message. "HTTP 404" did not say that the source had
moved on, nor that `update` fixes it.

## Fix

The updater's downloads now return a typed error (`FetchError`), so the
status code is kept instead of only a text. When `upgrade` gets a 404 or
410 for a file the stored index names (the system image or an app
package), the message says why and what to run:

```
updater: download failed: HTTP 404: the source no longer serves aricodeos-2.img, most likely replaced by a newer release since the last update: run update, then upgrade
```

For the development channel it says `update --channel dev`. The exit code
(41) and the event (`upd-image-download`, `upd-pkg-download`) do not
change. Nothing is retried automatically: installing a release the owner
has not seen reported would bypass the plan.

Two alternatives were rejected:

- Pointing the index at the release's own tag URL. Index file names are
  plain names inside the source's directory (`valid_file`), so that needs
  a signed-format change. It would also tie the index to GitHub's layout,
  and the owner's mirror (ADR 0048, decision 3) could no longer serve the
  same publication unchanged.
- Having `upgrade` fetch the index again first. That repeats `update`
  inside `upgrade`, and it still has to stop when the index changed, for
  the same reason as above.

## Verification

- Scenario `update.withdrawn_image` (the source at the `latest/download/`
  path with GitHub's redirects, the image removed after publishing):
  - before the fix, the bare message;
  - after the fix, the new one, with exit 41 and nothing installed.
- `cargo xtask test --all --area update`: 43 of 43 pass.
- `cargo xtask test --level unit`: passes.
- Logs are in `/home/serverbig/aricode-logs/0052/`.

## What remains

A source that publishes a new release still makes `upgrade` fail until
`update` runs again. The message now says so; the failure itself is by
design.
