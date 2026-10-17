# ADR 0029: Update sources and the signed update index

- **Date:** 2026-10-17
- **Status:** Accepted (extends ADR 0027, replaces its channel manifest and
  `update [URL|check]`)

## Context

ADR 0027 gave the machine one update channel: a URL in `updater/update_url`
and a `manifest.txt` with one system image. Installing an app from the
network still meant copying a package into `pkgs/` by hand. The owner asked
for something like apt, simpler and tightly controlled: several sources
that only the owner registers, an index of everything a source offers,
`update` to see what is new without touching anything, `upgrade` to install
it, no downgrades unless asked, and every install logged.

## Decision

1. **Sources are the owner's.** `update add <URL|IP[:PORT][/path]>`,
   `update remove <N|URL>` and `update sources` are shell commands; the list
   lives in `updater/sources` (one normalized URL per line, at most four,
   `aricode-update-index::Sources`). Only holders of the main file space can
   write it: the shell (console, or an SSH session authenticated by
   `authorized_keys`, ADR 0025) and the trusted updater and installer.
   Applications get their own `fs` prefix and cannot reach it. A source
   without a scheme is `http://`; other schemes, credentials, query strings
   and fragments are refused; one spelling per source (ports without
   leading zeros, trailing `/`), so duplicates are caught. Additions and
   removals are written to the install history with where they came from.
   A hand-damaged line is ignored and reported, never used.

2. **The index.** Each source publishes `index` and `index.sig`:

   ```text
   # AriCodeOS update index (ADR 0029)
   format = 1
   serial = 1792203676
   system = 2 aricodeos-2.img 2454712 <sha256>
   app = hello_ext 2 hello_ext-2.aripkg 14472 <sha256>
   ```

   At most one `system` and sixteen `app` entries (one per name); file names
   are plain (`[A-Za-z0-9._-]`, no leading dot) so an entry can never point
   outside its source; the parser is strict (unknown keys, extra fields and
   repeated keys are errors) so two readers cannot disagree on what was
   signed; at most 4 KiB. `index.sig` is the Ed25519 signature of the exact
   bytes of `index`, in hex (the format of `cargo xtask sign`, so `cargo
   xtask verify keys/dev/ed25519.pub index index.sig` checks it on a host).

3. **Signatures before anything else.** The updater verifies `index.sig`
   with the project public key before parsing a single line of the index
   (`aricode_update_index::verify`). A refused index is a fault event
   (`upd-index-refused`, ADR 0028), its source contributes nothing, and the
   index verified earlier for that source, if any, is kept (it is still
   authentic). Every entry keeps its own signature as well: the system image
   carries its signed manifest (Phase 30) and each package its signed
   manifest (ADR 0017). At install time the download must match the index
   (size and SHA-256), then its own signature and hashes, and the version
   inside must equal the version in the index; only then is anything
   written. The image is downloaded into the updater's memory buffer, not
   into the `UPDATE` partition, so no disk sector changes before the checks
   pass. The installer and the broker verify a package again before
   installing or launching it.

4. **`update` stores, `upgrade` re-verifies.** `update` (updater argument
   `REFRESH`) fetches every source's index, keeps the verified copies in
   `updater/index-N` (source URL, signature, exact bytes) and reports, for
   the system (version of the active slot, verified from disk) and for each
   installed app (`apps/*.aripkg` that verify, what the installer and the
   broker accept), the best offer and whether it is an upgrade. It installs
   nothing. `upgrade` (argument `UPGRADE`) verifies the stored copies again
   instead of trusting AriFS, takes for each item the newest version among
   the sources (first source on ties), and installs: the system image into
   the inactive slot through the Phase 30 path (header sector written last,
   then the slot is scheduled with three tries; `reboot to apply`), and app
   packages staged in `pkgs/NAME.aripkg`, after which the shell runs the
   existing installer. `upgrade` only upgrades apps that are installed; new
   apps are listed by `update` as available.

5. **No downgrade by default.** An offered version older than the installed
   one is refused (fault event `upd-downgrade-refused`, and the installer's
   `inst-downgrade-refused`) unless the owner types
   `upgrade --allow-downgrade`; an equal version is never reinstalled. A
   validly signed old image or package may have known flaws: replaying it
   must not roll the machine back. With the flag the installer accepts
   older versions for every package then pending in `pkgs/`.

6. **Rollback protection for indexes.** `serial` grows with every
   publication (`publish-update` uses the Unix time). An index older than
   the verified one stored for the same source is refused
   (`upd-index-stale`): a replayed old index cannot hide newer versions.

7. **Logging.** Successes go to `updates.log` in AriFS (system installs,
   app installs, source changes; the last 2 KiB are kept,
   `aricode_update_index::history`), shown by `cat updates.log`. Every
   failure and refusal is said on the console and recorded as a fault event
   with its code, listed by `diag` (ADR 0028). The updater's last output is
   in `updater/last.log`, shown to remote sessions.

8. **Capabilities.** The updater's manifest becomes `system, net, block,
   install`: the main file space replaces its private `fs` prefix so it can
   read `apps/`, stage `pkgs/` and write the history. It already holds
   `block`, which reaches the raw AriFS partition, so this adds no
   authority. `install` stays limited to the broker's trust list
   (`installer`, `updater`).

9. **Migration from ADR 0027.** When `updater/sources` does not exist and
   `updater/update_url` holds a valid URL, it becomes the first source (the
   updater and the shell both migrate, whichever runs first). Once a sources
   file exists, even an empty one, `update_url` is ignored. `manifest.txt`
   is no longer published or read; `update URL` and `update check` are
   replaced by `update add URL` and `update`; `update apply` (install what
   the `UPDATE` partition holds) remains. `cargo xtask usb-image
   --update-source URL` writes `updater/sources`.

10. **Publishing.** `cargo xtask publish-update [--version N | --no-system]
    [--app DIR:VERSION]...` writes one complete source into `build/update/`
    (emptied first): image, packages, `index`, `index.sig`; `serve-update`
    publishes and serves it. The scenarios publish into
    `build/update-test/` only (incident 0003), described by a
    `[host_update]` table (`system`, `apps`, `tamper_index`, `source`).

## The signing key

Images, packages and indexes are signed with one Ed25519 key. Today it is
the development pair in `keys/dev` (its seed is in the repository on
purpose, so anyone can sign for a test machine). The public key is compiled
into AriBoot, init (the broker), the installer and the updater. A
production key replaces it like this:

1. `cargo xtask keygen /secure/offline/dir` on a machine that never serves
   updates; the seed never enters the repository or the build host.
2. The release build uses that directory's `ed25519.pub` in place of
   `keys/dev/ed25519.pub` (one file; every verifier includes it at build
   time), and the machines are installed from that build.
3. Images, packages and the index are signed with the offline seed; the
   publishing host only receives signed files. `cargo xtask verify` checks
   them before they are served.

Machines built with the development key keep accepting development
signatures; there is no key rotation in the protocol yet (see below).

## Consequences

- Host unit tests (`libs/update-index`): index round trip, strict parsing,
  signature verification and tampering, rollback serials, version
  comparison and the downgrade rule, best offer across sources, source
  normalization, the source file and migration, stored copies, history.
  `xtask`: a published index verifies with the development key and a
  tampered one does not.
- QEMU scenarios: `update.sources`, `update.migrate_legacy_url`,
  `update.network` (system upgrade and reboot into the new slot),
  `update.tampered_index`, `update.refuse_downgrade`,
  `apps.upgrade_network`; `ssh.exec` and `update.apply` follow the new
  messages and manifest.
- The transport stays untrusted (ADR 0027, decision 2): HTTP is enough for
  integrity; HTTPS adds privacy and hides which versions a machine fetches.
- Limits: four sources (each keeps one stored index in AriFS, whose table
  holds 32 files), a 4 KiB index, sixteen apps per index, 32 KiB packages,
  app names that fit `pkgs/NAME.aripkg` in 32 bytes, the 4 MiB image buffer.
- Open: key rotation and revocation (a signed key list in the index),
  freshness against a source that keeps serving one old index forever (an
  expiry date needs a trusted clock), installing apps that are not yet
  installed (`update install NAME`), removing staged packages from `pkgs/`
  (AriFS has no delete yet).
