# ADR 0074: A base system and an optional desktop profile

- **Date:** 2026-11-03
- **Status:** Accepted (design). The owner, 2026-11-03, asked for an
  optional desktop that is installed afterwards, so the installer does not
  hold everything. They chose to keep the compositor in the base system.
  Its implementation comes in the phases below.

## Context

One image serves every machine today. Its initrd holds:
- the kernel's services and drivers;
- the shell, the installer and the updater;
- the compositor, with two graphical demos started by the `gui` command
  line (`run_gui`).

An application installed later is a signed package in
`apps/NAME.aripkg` (ADR 0017):
- `run installer` installs packages left in `pkgs/`;
- `upgrade` updates the installed ones from the signed index, which also
  lists available packages that are not installed (ADR 0029, 0064).

Since ADR 0072 phase B, a package may be up to 8 MiB.

The uses the owner has in mind pull in two directions:
- a robot or a server wants a small system, with no desktop to boot,
  update or attack;
- a desktop machine wants a terminal, an editor, files, settings and a
  launcher.

Putting everything in one image makes the robot carry the desktop. Making
two images doubles the releases, the sticks and the tests.

Veterans split it the same way. Debian has a minimal installation plus
tasks such as `task-desktop`. Fedora has a minimal install plus package
groups. Windows Server Core has optional features. Their lesson:
- the base must be useful alone;
- the optional part must be updated with the system, never left behind
  (Debian's partial upgrades are the failure case).

## Decision

### 1. The base system

**The image stays one image, built and signed as today.** It holds
everything a headless machine needs:
- kernel, init, fsd, the network and its drivers, sshd;
- the shell, the installer and the updater.

**It also keeps the compositor and `libs/gfx`.** They are small, and a
robot's screen or a kiosk can use them without the desktop. The base boots
into the shell session, as today.

**The graphical demos leave the image.** `gui_demo` becomes a test
package that the scenarios install, so `gui` keeps working in the tests.

### 2. A profile is a signed list of packages in the update index

- **The index gains one line per profile:**

  ```text
  profile = desktop launcher terminal editor files settings
  ```

  - It is covered by the index signature, so a profile only names packages
    the same source signs.
  - An older updater refuses an index line it does not know, so profiles
    go only in indexes for the release that adds them. That is the same
    rule as every index change (ADR 0029).
- **The owner follows a profile with `update profile add desktop`.**
  - The shell adds the name to `updater/profiles` and logs it in the
    install history.
  - What the profile installs is never recorded there. It is what the
    signed indexes say, read again on every run.
  - What the profile *installed* is recorded: the updater appends each
    app it stages for the profile to the profile's line
    (`desktop editor terminal`). Only those apps are the profile's to
    delete; an app the owner installed by other means never is.
- **`update` plans, and `upgrade` installs.**
  1. `update` plans every app a followed profile names and the machine
     lacks, and shows each one with its profile.
  2. `upgrade` downloads and stages those apps exactly as it does
     application upgrades (ADR 0029, 0064). A whole profile fits one run
     (`MAX_PROFILE_APPS` 8 ≤ `MAX_STAGED` 8).
  3. The installer installs each app with its own atomic `replace` (ADR
     0018).
- **After a cut, nothing is lost.** Each package is all-or-nothing, so a
  half-installed profile only ever lacks whole packages. The next
  `update` plans them again.
- **The profile follows the system.** `upgrade` updates the installed
  apps together with the system, as before. An app a source later adds
  to a followed profile is planned and installed too, so the desktop
  never falls behind.
- **`update profile remove desktop` stops following the profile.**
  - It deletes the apps recorded as installed for it (AriFS v2 `delete`,
    ADR 0072), except those another followed profile names: these move to
    that profile's record.
  - What the other profiles name comes from the stored index of each
    current stable source, read from that source's own slot and verified
    again with the project key (`stored::verified_all`, the same check
    `upgrade` makes). When a source has no verified index, the list
    changed since the last `update`, or a followed profile is offered by
    no source, nothing changes and the shell says to run `update`.
  - The decision is made before the list is saved, so a refusal can be
    retried. Every deletion is logged.
- **`update profiles`** lists the profiles the machine follows.
- **Offline machines (phase B).** `cargo xtask usb-image --profile
  desktop` will put the packages in the stick's `pkgs/` and follow the
  profile.
  - This needs a stick with AriFS v2, so 0.18 or later.
  - An older stick is refused with a message to write a new one.
- **Publishing.** `cargo xtask publish-update --profile NAME:APP,APP`
  writes the line.
  - An updater before 0.18 refuses an index with a line it does not
    know, and with it the system image that would have updated it. So a
    source may carry profiles only once every machine following it runs
    0.18 or later.
  - `publish-update` refuses `--profile` unless the publisher says so
    with `--all-machines-0.18`.
- **Only on AriFS v2.** On a volume still in v1, the updater neither
  plans nor stages profile apps, and says why. It tells the version by
  fsd answering `Delete` with `Unsupported` on v1 (ADR 0072).
- **Only with a sound installed list.** When the `[installed]` list is
  missing or damaged, profiles are not followed in that run, since a
  "missing" app could be one the list failed to name.

### 3. Each package may declare the application interface it needs

- **The level is a number of its own, not the release number.**
  - Each system is built with it (`aricode_ab::APPS_LEVEL`, 1 in 0.18).
    The number grows when what applications may rely on grows: a
    capability, a protocol, a syscall.
  - Release numbers cannot play this part, because stable and development
    builds count differently. Android separates its API level from its
    release name for the same reason.
- **A package manifest may carry `requires = N`:** the lowest level it
  runs on. It is signed with the rest of the manifest, and it is written
  only when set, so every existing package keeps its exact manifest and
  signature.
- **Three programs refuse a package that needs more, and say which level
  it needs:**
  - the updater refuses it before staging (`upd-pkg-requires`);
  - the installer refuses it before installing (`inst-requires-newer`,
    exit code 17);
  - the broker refuses to launch it. This covers a package installed
    under a newer system and still there after a rollback.
  - A system installed by the same `upgrade` runs only after the reboot,
    so the app is installed by the next `upgrade`.
- **Known cost.** The index does not carry `requires`, so `update` still
  plans such an app, and each `upgrade` downloads it before refusing it.
  An `app =` field for it belongs to a later index format.
- **Developers** set the line with `cargo xtask app-pack DIR --requires
  N`.
- **Older systems never install such a package.** An older manifest
  parser refuses the unknown key, which is the safe failure.

### 4. The session

- **The installed desktop profile writes `session = desktop` into
  `session`.**
  - At boot, init starts the compositor and the launcher instead of the
    shell.
  - `session shell` (from the shell or over SSH) writes it back. The
    owner can always return to a text console: the physical keyboard's
    console stays reachable, as with `console_bridge` today.
- **Without the profile, the file is ignored.** A machine never boots
  into a desktop that is not installed.

### 5. The desktop profile's packages

These are ordinary applications with the `window` capability (ADR 0052,
0053). They never get raw input, and each one gets only what its manifest
declares.

| Package | What it is | Capabilities |
|---|---|---|
| `launcher` | Taskbar and application list; starts installed applications through the broker. | `window` and a new `launch` |
| `terminal` | A shell in a window, through a session console (ADR 0047). | `window`, a session shell from init |
| `editor` | A text editor for AriFS files. | `window`, `fs` |
| `files` | Lists, opens, renames and deletes files. | `window`, `fs` |
| `settings` | Network, update source, session, enrolled driver keys (read-only). | `window`, `system` |

- **`launch` is a new capability.** It asks the broker to start an
  installed application with that application's own manifest, never with
  the launcher's rights.
  - Like `install` for `installer`, the broker grants it only to
    `launcher`, and only when the package is official.
  - It is the only new privilege the profile adds. No service and no
    driver comes from a profile (the owner's choice: the compositor
    stays in the base).
- **`terminal` needs a session shell.** init grants one per window, as it
  does for sshd's sessions (ADR 0025): the shell keeps its own fsd
  session and nothing passes through the terminal's manifest.

## Implementation phases

| Phase | Content |
|---|---|
| A (done) | The `profile` index line; `update profile add/remove` and `update profiles`; `update` and `upgrade` following profiles, with the apps they install recorded; `requires` checked by the updater, the installer and the broker. Scenarios `apps.profile`, `apps.profile_v1` and `apps.requires_newer`. |
| B | `session = desktop` in init; `launch` in the broker with its trust rule (audited: it touches the broker's grants); `gui_demo` as a test package; `usb-image --profile`. |
| C | The desktop applications, one branch each: launcher, terminal, editor, files, settings. The widgets they share go into `libs/gfx`, with host tests. |

## Audit of phase A (2026-11-03)

An independent read-only audit found no high findings. These were fixed
in the same branch:

- **M1:** removal read stored indexes without binding each to its current
  source. It now uses the same slot-bound check as `upgrade`, shared in
  `stored::verified_all`.
- **M2:** removal deleted apps the owner had installed by other means.
  Each profile now records what it installed, and only that is deleted.
- **M3:** the broker did not check `requires`, so a rollback could run a
  package made for a newer interface. It now refuses to launch it.
- **M4:** a `profile` line strands updaters before 0.18. `publish-update`
  now needs `--all-machines-0.18`.
- **Low findings:**
  - L1: an unknown followed profile now stops a removal.
  - L2: profiles are not followed on AriFS v1.
  - L3: the installer empties consumed stage files.
  - L5: the installer's exit code is 17 for a `requires` refusal.
  - L6: `requires` has one spelling.
  - L7: the deletion list is computed before saving, and failures are
    said.
  - L8: profiles are not followed without a sound installed list.
  - L9: the docs were updated, the unused `apps` key in the features
    file was dropped, and `app-pack --requires` was added.
  - L10: new tests.
- **Left as known:** L4, the repeated download of a refused package.

## Consequences

- **A headless machine carries no desktop code** beyond the compositor
  and `libs/gfx`, and it updates nothing it does not use.
- **One image, one release process and one set of sticks** serve every
  profile. A profile is packages signed by the same key.
- **The desktop moves at the system's pace.** `upgrade` updates the
  system and its profile together, and `requires` stops a package from
  running on a system too old for it.
- **Only machines on 0.18 or later get profiles**, because they need
  AriFS v2: a machine still on AriFS v1 holds packages of 32 KiB at most.
  Sticks written before 0.18 have to be rewritten (ADR 0072, the ESP
  rule).
- **More profiles can follow without new mechanisms,** for example a
  robotics one with serial and CAN tools once those drivers exist.
  Drivers that are not built in arrive as signed driver packages (ADR
  0073), not through profiles.
