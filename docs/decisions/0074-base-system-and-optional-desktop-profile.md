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
  - The shell records only the name, in `updater/profiles`, and logs it
    in the install history.
  - What the profile installs is never recorded. It is what the signed
    indexes say, read again on every run.
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
  - It deletes the apps the profile installs that no other followed
    profile installs (AriFS v2 `delete`, ADR 0072). The list comes from
    the stored indexes of the stable sources, verified again with the
    project key.
  - Every deletion is logged.
  - Without a verified index naming the profile, nothing is deleted, and
    the shell says so.
- **`update profiles`** lists the profiles the machine follows.
- **Offline machines (phase B).** `cargo xtask usb-image --profile
  desktop` will put the packages in the stick's `pkgs/` and follow the
  profile.
  - This needs a stick with AriFS v2, so 0.18 or later.
  - An older stick is refused with a message to write a new one.
- **Publishing.** `cargo xtask publish-update --profile NAME:APP,APP`
  writes the line. Profiles only go in indexes for systems that read
  them.

### 3. Each package may declare the application interface it needs

- **The level is a number of its own, not the release number.**
  - The system declares `apps = N` in its initrd's `features` file
    (`aricode_ab::APPS_LEVEL`, 1 in 0.18). The number grows when what
    applications may rely on grows: a capability, a protocol, a syscall.
  - Release numbers cannot play this part, because stable and development
    builds count differently. Android separates its API level from its
    release name for the same reason.
- **A package manifest may carry `requires = N`:** the lowest level it
  runs on. It is signed with the rest of the manifest, and it is written
  only when set, so every existing package keeps its exact manifest and
  signature.
- **Both programs refuse a package that needs more, and say which level
  it needs:**
  - the updater refuses it before staging (`upd-pkg-requires`);
  - the installer refuses it before installing (`inst-requires-newer`).
  - A system installed by the same `upgrade` runs only after the reboot,
    so the app is installed by the next `upgrade`.
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
| A (done) | The `profile` index line; `update profile add/remove` and `update profiles`; `update` and `upgrade` following profiles; `requires` with the `apps` level. Scenarios `apps.profile` and `apps.requires_newer`. |
| B | `session = desktop` in init; `launch` in the broker with its trust rule (audited: it touches the broker's grants); `gui_demo` as a test package; `usb-image --profile`. |
| C | The desktop applications, one branch each: launcher, terminal, editor, files, settings. The widgets they share go into `libs/gfx`, with host tests. |

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
