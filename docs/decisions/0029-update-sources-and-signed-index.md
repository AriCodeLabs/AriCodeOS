# ADR 0029: Update sources and the signed update index

- **Date:** 2026-10-17 (revised the same day after audit 1,
  `docs/audits/update-sources-audit-1.md`)
- **Status:** Accepted (extends ADR 0027, replaces its channel manifest and
  `update [URL|check]`); extended by ADR 0045 (the development channel:
  `dev` sources, `channel`/`commit` in the index and the system manifest,
  a second key, `--channel` and `update rollback`)

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
   `authorized_keys`, ADR 0025) and the installer; the updater reaches it
   through its own private directory. Applications get their own `fs`
   prefix and cannot reach it. A source without a scheme is `http://`;
   scheme and host are lower-cased, the default port is dropped, other
   schemes, credentials, query strings, fragments, `..`/`.` segments and
   `//` in the path are refused: one spelling per source, so duplicates
   are caught. Additions and removals are written to the install history
   with where they came from. A hand-damaged line is ignored and reported
   (fault event), never used.

2. **The index.** Each source publishes `index` and `index.sig`:

   ```text
   # AriCodeOS update index (ADR 0029)
   format = 1
   serial = 1792203676
   system = 2 aricodeos-2.img 2454712 <sha256>
   app = hello_ext 2 hello_ext-2.aripkg 14472 <sha256>
   ```

   At most one `system` and sixteen `app` entries (one per name, names of
   at most 20 bytes so `apps/NAME.aripkg` fits AriFS's 32-byte names); file
   names are plain (`[A-Za-z0-9._-]`, no leading dot) so an entry can never
   point outside its source; the parser is strict (unknown keys, extra
   fields and repeated keys are errors) so two readers cannot disagree on
   what was signed; at most 4 KiB. `index.sig` is the Ed25519 signature of
   the exact bytes of `index`, in hex (the format of `cargo xtask sign`, so
   `cargo xtask verify KEY.pub index index.sig` checks it on a host).

3. **Signatures before anything else.** The updater verifies `index.sig`
   with the project public key before parsing a single line of the index
   (`aricode_update_index::verify`). Both downloads are bounded (4 KiB and
   256 bytes: the HTTP sink refuses anything beyond) and the whole run has
   a ten-minute deadline (`upd-deadline`), carried as a budget into every
   network operation of every download (resolve, connect, TLS handshake,
   each send and receive, interim responses, headers and chunk framing
   included) since incident 0009 (ADR 0033). A refused index is a fault event
   (`upd-index-refused`, ADR 0028), its source contributes nothing, and the
   index verified earlier for that source, if any, is kept (it is still
   authentic). Every entry keeps its own signature as well: the system image
   carries its signed manifest (Phase 30) and each package its signed
   manifest (ADR 0017). At install time the download must match the index
   (size and SHA-256), then its own signature and hashes, and the version
   inside must equal the version in the index; only then is anything
   written. The image is downloaded into the updater's memory buffer, not
   into the `UPDATE` partition. The installer and the broker verify a
   package again before installing or launching it.

4. **`update` reports and plans, `upgrade` follows the plan.** `update`
   (updater argument `REFRESH`) first has the installer write the installed
   apps to the `[installed]` section of `updater/state` (installer `LIST`), then fetches every
   source's index, keeps the verified copies in `updater/indexes` (per source slot:
   URL, signature, exact bytes), reports for the system (version of the
   active slot, verified from disk) and for each installed app the best
   offer, and saves everything that differs as the `[plan]` section. It installs
   nothing. `upgrade` (argument `UPGRADE`) verifies the stored copies
   again instead of trusting AriFS and installs only items of the plan at
   the version the plan names (`upd-not-reported` otherwise): the system
   image into the inactive slot through the Phase 30 path (the old header
   is zeroed and flushed first, the new header written last, then the slot
   is scheduled with three tries; `reboot to apply`), and app packages
   staged in the updater's directory as `pkg-1.aripkg`..`pkg-4.aripkg`
   with the `[staged]` section listing file, name, version and SHA-256. The shell
   then runs the installer with `STAGED`, which installs exactly those
   files after checking them against the list and their own signature, and
   empties the list. `upgrade` only upgrades installed apps; new apps are
   listed by `update` as available. The disk is only opened when a system
   image is involved, so apps-only work needs no A/B partitions.

5. **No downgrade by default.** An offered version older than the
   installed one is refused (fault events `upd-downgrade-refused`,
   `inst-downgrade-refused`) unless the owner types
   `upgrade --allow-downgrade` (or `update apply --allow-downgrade` for the
   `UPDATE` partition); an equal version is never reinstalled. A system
   whose installed version cannot be read or verified counts as a
   downgrade (`upd-installed-unknown`): the machine never replaces what it
   cannot identify without being told to. A validly signed old image or
   package may have known flaws: replaying it must not roll the machine
   back. The installer accepts `ALLOW_DOWNGRADE` only together with
   `STAGED` (`inst-flags-refused` otherwise), so the flag never reaches
   packages left in `pkgs/` by hand, and a staged list is consumed once.

6. **Rollback protection for indexes.** `serial` grows with every
   publication: `publish-update` uses the Unix time but never less than
   the previous publication's serial plus one. An index older than the
   verified one stored for the same source is refused (`upd-index-stale`):
   a replayed old index cannot hide newer versions. A serial pushed far into
   the future by a clock error is recovered on each machine with `update
   remove N`, `update` (which drops the stored copies of sources no longer
   listed) and `update add URL`.

7. **Logging.** Successes go to `updater/history.log` (system installs by
   the updater, app installs by the installer, source changes by the
   shell; the last 2 KiB are kept, `aricode_update_index::history`), shown
   by `cat updater/history.log`. Every failure and refusal is said on the
   console and recorded as a fault event with its code, listed by `diag`
   (ADR 0028): missing partitions, memory, key, services, an invalid
   `update_url`, a list changed since the last update, a source without a
   verified index, an installed package failing verification (listed by
   the installer, never replaced silently), and every download or
   verification failure. A structurally damaged `updater/state`
   (repeated or unknown section headers, lines outside a section;
   incident 0006) is repaired and saved at once when it is read, and
   recorded as `upd-state-repaired` (updater, code 47) or
   `inst-state-repaired` (installer, code 13): a repeated `[plan]` or
   `[staged]` is dropped (both are recomputed by running `update` and
   `upgrade` again), a repeated `[installed]` keeps its last copy (it is
   rewritten by the installer before every updater run). Line endings,
   blank lines and a missing final newline are formatting, not damage.
   A request the file service does not answer is never read as an absent
   file or an empty directory (incident 0008,
   `aricode_update_index::files`): `updater/state` is then
   `upd-state-unreadable` (updater, code 38) or `inst-state-unreadable`
   (installer, code 12) and is not rewritten; an installed copy the
   installer cannot read is `inst-installed-unknown` (code 14: the package
   is refused and the installed list is not written); a directory it
   cannot list is `inst-dir-list` (code 15: nothing is installed or
   listed); a pending or staged package it cannot read is `inst-pkg-read`
   (code 16, distinct from missing or tampered); a history that cannot be
   read keeps its old lines and the new one is only said: `inst-history-read`
   (installer, code 5), `upd-history-read` (updater, code 48),
   `sh-history-read` (shell, code 2); all three append through
   `aricode_update_index::history::appended`. The installer and the
   updater read `updater/state` once per run and pass the text down, so an
   unreadable state is one fault per run; a `STAGED` run that hit a
   failure (as opposed to a refusal) keeps the `[staged]` list and exits
   nonzero, and the updater compares no apps and saves no plan when the
   state could not be read. A package whose name is not a valid app name
   (decision 2, `aricode_update_index::installed_path`) is refused as a
   package (`inst-pkg-name`, code 7), never reported as a file-service
   failure. The updater's last output is in `updater/last.log`, shown to
   remote sessions.

8. **Capabilities (least privilege).** The updater keeps `system, fs,
   net, block`: its `fs` grant is its private directory `updater/`. It
   does not get the main file space: it parses network input, and `install`
   would give it journaled writes to `authorized_keys`, the SSH host key,
   `trusted_ca.der` and `apps/`. Installed versions reach it in
   `[installed]` of `updater/state`; staged packages leave through its `[staged]` section; the
   installer (main space, `install`, trust list `installer` only) does the
   installing. Follow-up: the raw `block` grant still reaches every
   partition, ARI0 included; it should be limited to `UPDATE`, `ARIBOOT`
   and the slot partitions by the block service.

9. **Migration from ADR 0027 (transition release).** When
   `updater/sources` does not exist and `updater/update_url` holds a valid
   URL, it becomes the first source (the updater and the shell both
   migrate, whichever runs first); an invalid one is refused visibly
   (`upd-legacy-url-invalid`). Once a sources file exists, even an empty
   one, `update_url` is ignored. Machines still running the ADR 0027
   updater only read `manifest.txt`: for this release `publish-update`
   also writes it next to the image by default, so they can fetch this
   build over the network with their `update`; after that they speak the
   new protocol and `--no-legacy-manifest` drops it. Because an ADR 0027
   updater installs any version that differs from its own, older ones
   included, `publish-update` refuses to write `manifest.txt` for a system
   version below the one already published. After the transition release
   the legacy manifest becomes opt-in (the default flips to no
   `manifest.txt`, and a `--legacy-manifest` flag asks for it). `update URL` and
   `update check` are replaced by `update add URL` and `update`;
   `update apply` remains. `cargo xtask usb-image --update-source URL`
   writes `updater/sources`.

10. **Publishing.** `cargo xtask publish-update [--version N | --no-system]
    [--app DIR:VERSION]... [--no-legacy-manifest] [--signing-key DIR]`
    builds one complete source in a sibling directory and moves it into
    `build/update/` at the end, so a machine fetching meanwhile never sees
    a mix of two publications; `serve-update` publishes and serves it. The
    scenarios publish into `build/update-test/` only (incident 0003),
    described by a `[host_update]` table (`system`, `apps`,
    `tamper_index`, `tamper_image`, `stale_index`, `source`).

## The signing key

Images, packages and indexes are signed with one Ed25519 key, and the
build embeds its public half in every verifier (AriBoot, init's broker, the
installer, the updater) through `aricode_crypto::trust`.

The development pair in `keys/dev` is the default. Its seed is in the
repository on purpose, so anyone can sign images and packages that a
dev-key build accepts: anyone on the LAN can then upgrade such a machine.
xtask prints a warning whenever it signs with it. A production key is
required before network upgrades on untrusted networks:

1. `cargo xtask keygen /secure/offline/dir` on a machine that does not
   serve updates. The seed never enters the repository: xtask refuses a
   `--signing-key` directory inside it, and checks that seed and public key
   are a pair. The verified public key is kept for the whole run and never
   read again.
2. `cargo xtask usb-image --signing-key /secure/offline/dir` builds the
   system with that public key embedded (`ARICODE_SIGNING_PUB`; cargo
   rebuilds the verifiers when it changes) and signs slot A with it. The
   machines are installed from that image.
3. `cargo xtask publish-update --signing-key /secure/offline/dir ...` (or
   `serve-update`) builds with the same key and signs the image, the
   packages and the index. A machine built with one key refuses everything
   signed with another.

After every build xtask checks that AriBoot, init, the installer and the
updater carry the trust anchor with exactly the selected key, and that the
system images and packages it signs verify with it; a mismatch fails the
build. The key is decoded at compile time, so an invalid
`ARICODE_SIGNING_PUB` does not build.

There is no key rotation in the protocol yet (see below).

Since ADR 0045 there is a second key, the development channel's
(`keys/dev-channel/`, seed in `~/.config/aricodeos/dev-signing-key`): it
signs development builds and their indexes only, the key above never
signs them, and every rule of this section applies to it the same way
(`--signing-key DIR --channel dev`). The updater of every build carries its
public half, inert unless the machine opted in; AriBoot carries it only on
a machine installed with `usb-image --dev-source`.

## Consequences

- Host unit tests (`libs/update-index`): index round trip, strict parsing,
  app name length, signature verification and tampering, rollback serials,
  version comparison and the downgrade rule, best offer across sources,
  source normalization, the source file and migration (including an
  invalid `update_url`), stored copies, the installed/plan/staged lists
  (stage files only), history, and optional-file reads over a mock file
  service (absent, failed open, failed read, oversized, a failed listing,
  and the state rewrite keeping its other sections; incident 0008), the
  history append over the same mock (absent, failed read, an overgrown log
  trimmed, an absurd one refused), the installed path (the longest valid
  name fits AriFS exactly; longer or invalid names give no path).
  `xtask`: a published index verifies with the
  development key and a tampered one does not, serials only grow,
  publication leaves no staging directory, signing keys must be a pair
  outside the repository, the post-build anchor check, the legacy
  manifest never going below the published version. `aricode-crypto`:
  the embedded key and the compile-time decoder.
- Installer memory: the 64 KiB quota is DMA only (the file client's 4 KiB
  buffer); packages use the runtime's 256 KiB static heap, whose peak
  during an install is under 80 KiB (one 32 KiB package, the installed copy
  read for its version, the history and a list).
- QEMU scenarios: `update.sources`, `update.migrate_legacy_url`,
  `update.network` (system upgrade and reboot into the new slot),
  `update.tampered_index`, `update.tampered_image`, `update.stale_index`,
  `update.refuse_downgrade`, `apps.upgrade_network`,
  `apps.downgrade_refused`, `apps.downgrade_allowed`,
  `apps.staged_tampered`, `update.installed_unknown`,
  `update.apply_refuses_older`, `update.fs_nearly_full`; `ssh.exec` and
  `update.apply` follow the new messages.
- The transport stays untrusted (ADR 0027, decision 2): HTTP is enough for
  integrity; HTTPS adds privacy and hides which versions a machine fetches.
  The updater trusts only the embedded root certificates for HTTPS.
- AriFS table (32 entries in all, re-audit R3): the updater's directory
  uses `sources`, `indexes` (every stored copy), `state` (installed, plan
  and staged sections), `last.log`, `history.log` and up to four stage
  files, plus `update_url` on migrated machines. A first `update` creates
  `state` (installer `LIST`), `indexes` and `last.log`; an `upgrade` adds
  `history.log` after a system install and one stage file per package:
  five free entries for the system and one app (`update.fs_nearly_full`;
  the first QEMU run of that scenario exposed init's `fs_client` taking
  one more at boot for `nuevo.txt`; since ADR 0032 that test client runs
  only with `selftest`, and the nearly-full scenarios boot without it).
  `upgrade` decides everything first and reserves (creates empty) every
  entry it will need before the first irreversible step: if the table
  cannot hold them it refuses the whole upgrade (`upd-fs-full`,
  `update.fs_one_short`), so a full table never leaves the system
  scheduled and an app not staged. A download or verification failure
  after the system install can still leave apps unstaged; that is
  reported as its own fault. A `state` or
  `indexes` file that cannot be read (or is oversized) is a fault and is
  not rewritten in that run, so neither the exchanged lists nor the stored
  serials are reset silently; "cannot be read" includes a lost reply or a
  timeout from the file service, which is never taken for a missing file
  (incident 0008).
- Limits: four sources, four staged packages per `upgrade`, a 4 KiB index,
  sixteen apps per index, 32 KiB packages, app names of 20 bytes, the
  4 MiB image buffer.
- Open: key rotation and revocation (a signed key list in the index);
  freshness against a source that keeps serving one old index forever (an
  expiry date needs a trusted clock); binding an index to the source that
  serves it (a validly signed index can be served by any registered
  source); installing apps that are not yet installed (`update install
  NAME`); limiting the updater's raw `block` grant to its partitions.
