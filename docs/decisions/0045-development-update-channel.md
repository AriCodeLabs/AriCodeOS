# ADR 0045: Development update channel

- **Date:** 2026-10-23
- **Status:** Accepted (extends ADR 0029 and ADR 0027; uses the A/B
  policy of Phase 30)

## Context

Hardware testing, of drivers above all, happens on the reference laptop.
Until now a build reached it only through the stable path: `publish-update
--version N` with the release key and a version number picked by hand,
always above the one installed (ADR 0029, decision 5, refused publishing
version 2 below 11), or by rewriting the USB stick. The owner asked for a
development channel that puts a `develop` or branch build on the laptop in
minutes, before anything reaches `main`, without hand-picked numbers, and
without weakening what protects every other machine: sources are the
owner's, indexes are signed and never replayed, nothing is downgraded
without being asked, doors are closed by default, refusals are logged.

## Decision

1. **Two channels.** `stable` carries releases from `main` (what published
   images and sticks use, signed with the release key) and `dev` carries
   builds of `develop` or a branch. A source declares its channel in
   `updater/sources`: a plain line is a stable source, as before; a
   development source is the same normalized URL after `dev `
   (`dev http://192.168.1.10:8081/`), one URL in one channel only. `update`
   and `upgrade` act on the stable sources by default; `update --channel
   dev` and `upgrade --channel dev` act on the development ones
   (`update --channel stable` and `upgrade --channel stable` name the
   default explicitly). The channel is **closed by default**: no machine
   has a development source unless its owner adds one (`update add
   --channel dev URL`, console or authenticated SSH, recorded in the
   history as `dev source added`), and a stick gets one only from an
   explicit `cargo xtask usb-image --dev-source URL`. Without one,
   `update --channel dev` is refused and recorded (`upd-no-dev-source`).
   A development index offers one system image and no apps; apps keep
   coming from stable sources.

2. **Build numbers, commits and switches.**
   - `cargo xtask publish-update --channel dev` (and `serve-update
     --channel dev`) publishes into `build/update-dev/` and numbers the
     build itself: one above the last build published from this host, by
     any checkout or worktree, as the host counter
     (`~/.cache/aricodeos/dev-build`, locked for the whole publication),
     the directory's index and the counter beside it
     (`update-dev.last-build`) say, whichever is highest (audit M2). `--version`, `--no-system`
     and `--app` are refused with `--channel dev`; a test that names a
     number must still go above the last one. It records the checkout's
     commit (`git rev-parse --short=12 HEAD`, `-dirty` with uncommitted
     tracked changes, which is said).
   - The signed system manifest of a development build names its channel
     and commit after the version, which is the build number; a release
     manifest is unchanged (exactly the three keys every loader since
     Phase 30 parses):

     ```text
     version = 7
     channel = dev
     commit = 0123abc
     kernel = <sha256>
     initrd = <sha256>
     ```

     The parser is strict: no `channel = stable` (one spelling), a
     development manifest without its commit or a release with one is
     refused, repeated keys are refused.
   - A development index carries the same after `serial`, and its single
     `system` entry's version is the build number
     (`system = 7 aricodeos-dev-7.img ...`). The updater checks that the
     downloaded image's manifest says the same channel, build and commit
     as the index.
   - AriBoot adds `boot_version=N boot_channel=stable|dev` and, for a
     development build, `boot_commit=HEX` to the command line of the slot
     it verified, after dropping any copy of its keys from `cmdline.txt`.
     `diag` prints `system   dev build 7 (commit 0123abc), slot B` (or
     `version 12`, or the ESP fallback); `update` names the installed
     system the same way, read from the active slot and verified.
   - Within a channel the ADR 0029 rules are unchanged: serials only grow
     per source (anti-replay), an older build or version is refused
     without `--allow-downgrade`, the same one is never reinstalled. A
     build number and a release version are not comparable, so **across
     channels** the rule is the owner's word: an image of the other
     channel is installed only by `upgrade --channel C` naming that
     channel, whatever the numbers say (stable 2 replaces development
     build 5), but never below the **stable floor**: the newest stable
     release this machine ran, kept in the `[floor]` section of
     `updater/state` (raised whenever the active system is a newer stable
     release, never lowered, seeded by `usb-image` from the stick's
     release). A stable release below it is a downgrade, switch or not,
     refused without `--allow-downgrade` by `upgrade` and `update apply`
     alike (`upd-downgrade-refused`, audit M1): a first, unauthenticated fetch of an old validly signed stable
     index cannot roll a development machine back past what it ran. It is
     said and recorded once installed (`upd-channel-switch`, code 52),
     and the history line names both systems.
   - A development build number seen again with another commit (the same
     number published twice) is never taken for the installed build nor
     installed over it: refused (`upd-build-conflict`, code 57) until a
     new publication gives it a new number. A plain `upgrade` on a
     development build never leaves the channel: it is refused and
     recorded (`upd-channel-switch-refused`, code 53). `update apply`
     never changes the channel either: an image of the other channel is
     refused, and with an unknown installed version (`--allow-downgrade`)
     it takes a stable image only (audit L4).
   - The plan records the channel it was made for (`channel dev`, then
     `system dev 7`; a stable plan is written as before): `upgrade
     --channel C` follows only a plan of `C` (`upd-plan-channel`
     otherwise).

3. **Signing: the key is the channel.** The development channel has its
   own Ed25519 key, generated with `cargo xtask keygen` into
   `~/.config/aricodeos/dev-signing-key` (outside the repository, seed
   mode 600, directory mode 700) with its public half committed in
   `keys/dev-channel/`. The release key never signs a development build
   and the development key never signs a release: a manifest or an index
   whose channel differs from its key's is refused
   (`ImageError::ChannelMismatch`, `IndexError::ChannelMismatch`), and a
   source only accepts its channel's key
   (`aricode_update_index::verify_channel`).
   - **Where the public key lives.** We chose least privilege at the
     place that survives updates. AriBoot sits on the ESP and is never
     replaced by the updater, so it is the per-machine anchor: it carries
     the development key only on a machine installed from a stick built
     with `--dev-source` (and in test builds, whose seeds are public
     anyway). AriBoot writes on every boot whether it trusts the channel
     (`BootState::loader_dev_channel`, byte 16 of the boot state; a loader
     before this ADR leaves it zero). The updater of every build carries
     the public key, inert unless the machine's loader says it trusts
     the channel: only then is the key part of the run's keys
     (`aricode_update_index::trusted_keys`), used to verify the installed
     system and development indexes and images. Development indexes are
     only fetched from development sources, and a `--channel dev` run
     without one is refused, so the updater never accepts an index or
     installs a build the loader would refuse. The flag is advisory, not a trust root: forging it only makes
     the updater install an image AriBoot then rejects (and rolls back
     from). init's broker and the installer never carry it: the channel
     has no apps. xtask checks all of this after every build (`check_dev_anchor`).
   - *Rejected: the key in every AriBoot.* The development key is used
     daily on the build host; a leak would let anyone with the means to
     write a slot boot code on every machine, not only the owner's
     laptop.
   - *Rejected: the key only in builds made with `--dev-source`.* A
     stable release is built once for everyone; its updater would not
     know the development key, so a laptop that went back to stable could
     only return to the development channel by rewriting its stick, which
     is what the channel exists to avoid.
   - **Rules enforced in code.** `aricode_crypto::trust` refuses to
     compile a development key equal to the release key, and, in a build
     with a real `ARICODE_SIGNING_PUB`, one whose seed is public (the test
     key `keys/dev` and the development channel's test pair
     `keys/dev/dev-channel`). xtask selects keys per run: `--signing-key
     DIR --channel dev` must be the committed development key and embeds
     `keys/release` as the stable anchor, so a development build can go
     back to stable; `--signing-key DIR` for the stable channel refuses
     the development key and any test key; without `--signing-key` the
     test pairs sign both channels, with a warning, and naming the test
     directories themselves (`--signing-key keys/dev`, or
     `keys/dev/dev-channel` with `--channel dev`) is the same test mode,
     with the same warning. The committed release key a development build
     embeds must not be a test key, and `aricode_crypto::trust` refuses to
     compile a real `ARICODE_SIGNING_PUB` equal to either test key. The public test key
     `keys/dev` is therefore never accepted by any non-test channel.

4. **Safety.** A development build that does not boot is reverted by the
   A/B policy as before (three tries, or an immediate rollback when its
   image does not verify). For one that boots but breaks the network,
   `upgrade --channel stable` cannot help, so the shell gains `update
   rollback` (updater argument `ROLLBACK`): it verifies the image still in
   the inactive slot (the system before the last install) with the keys
   this machine's loader trusts and schedules it with three tries, as an
   install would; after `reboot` init confirms it and it becomes active.
   It says what it boots: another channel, or an older release (a
   downgrade, below the stable floor too: this is the owner's local,
   explicit way back, audit L3). That slot holds the system before the
   last install, unless that install never stayed (the loader rejected
   it or ran out of tries): it then holds the failed image, and rolling
   back retries it.
   No network, no download; it writes only the boot state, the history
   and its own log. Refused and recorded when that slot holds no bootable
   image (`upd-rollback-none`) or an install is already pending
   (`upd-rollback-pending`, both code 55); a rollback is recorded
   (`upd-rollback`, code 54) and written to the history.

5. **Open (phase 2, not implemented).** Single-component development
   updates (a driver or the initrd alone, without a whole system image)
   would shorten the loop further but need their own signed manifest and
   a rule for mixing components across channels.

## Consequences

- Host tests: `aricode-ab` (development manifest round trip and strict
  rules, the longest commit fits, the signing key decides the channel,
  the loader flag survives confirm/schedule and only 0 or 1 decode),
  `aricode-update-index` (development index round trip and rules, each
  channel accepts only its key, a machine without the key refuses the
  channel, `dev` source lines and damaged ones, per-channel plans, the
  switch rule), `aricode-cmdline` (loader keys dropped from `cmdline.txt`),
  `aricode-crypto` (the anchor follows the variable), xtask (key rules
  per channel with real and test keys, the committed keys, the anchor
  check, build numbers only grow, a development source offers one image,
  the recorded commit text).
- QEMU scenarios (area `update`): `update.dev_channel` (closed by
  default, opt-in source, `upgrade --channel dev`, `diag` and the boot
  log name the build, a plain `upgrade` refuses to switch, `upgrade
  --channel stable` installs a lower stable, the history),
  `update.dev_wrong_key` (a development index signed with the release key
  is refused and recorded, nothing installed), `update.dev_rollback`
  (`update rollback` from a development build, refused when nothing to
  roll back to or already pending). The scenario table `[host_update]`
  gains `dev_build`, `dev_commit`, `dev_wrong_key` and `dev_source`; the
  test server serves the development source at `/dev/`.
- Not covered in QEMU: a loader that does not trust the channel (the test
  build's AriBoot trusts the test pair, one build serving every
  scenario). The refusal path is short (`run_keys`, `dev_not_trusted`)
  and the anchors are checked after every build.
- Removing `~/.cache/aricodeos/dev-build` and every `build/update-dev`
  restarts the numbering: a machine on a higher build refuses the lower
  number as a downgrade until the owner types `upgrade --channel dev
  --allow-downgrade` once.
- The laptop installed before this ADR has an AriBoot without the
  development key: it joins the channel once, from a stick written with
  `cargo xtask usb-image --signing-key ~/.config/aricodeos/signing-key
  --dev-source URL` (docs/hardware.md, "Development builds in minutes").
- Compatibility: a release built before this ADR, if a machine goes back
  to it, reads a `dev URL` line as a damaged source line and a development
  plan as damaged plan lines (both refused and recorded, never used), and
  its loader ignores byte 16 of the boot state; stable manifests, indexes
  and source lines are written exactly as before.
- New fault codes of the updater: 52 `upd-channel-switch` (a switch the
  owner asked for), 53 `upd-channel-switch-refused`, 54 `upd-rollback`,
  55 `upd-rollback-none`/`upd-rollback-pending`, 56 `upd-dev-not-trusted`,
  57 `upd-build-conflict`, 58 `upd-floor-damaged`; `upd-no-dev-source`
  uses 21, `upd-plan-channel` 37, a stable release below the floor 40.
- Audit follow-up (2026-10-23): the rules above are pure functions of
  `aricode-update-index` with host tests (`decide_system`, `raise_floor`,
  `trusted_keys`, `image_matches_offer`); scenarios
  `update.stable_floor`, `update.apply_stable_floor` and
  `update.dev_stable_image` (a development
  index pointing at a stable image is refused at install) join the list
  above. The boot command line readers take 1 KiB and report a line that
  does not fit (`console.long_cmdline`), since AriBoot's keys lengthen
  it.
- Open, besides phase 2: key rotation and revocation remain open for
  both keys (ADR 0029).
- Note (2026-10-27, ADR 0060, incident 0045): a damaged `[floor]` no
  longer counts as no floor. It refuses every stable release
  (`upd-floor-refused`, code 58) until a stable install the owner forced
  with `--allow-downgrade` rewrites it as the running stable release once
  it went in (`upd-floor-rewritten`).
