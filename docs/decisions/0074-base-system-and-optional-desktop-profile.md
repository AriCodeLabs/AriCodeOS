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
- **`install --profile desktop` (in the installer) installs a profile:**
  1. It downloads each package through the updater, as `upgrade` does for
     applications (ADR 0064).
  2. It installs each one with its own atomic `replace` (ADR 0018).
  3. It records the profile in `profiles` only when every package is in.
  4. After a cut, the next run finds the profile unrecorded and installs
     the packages still missing. Each package is all-or-nothing, so a
     half-installed profile only ever lacks whole packages.
- **`upgrade` updates the packages of every recorded profile** together
  with the system. A profile package the index adds later is installed
  too, so the desktop never falls behind the system.
- **`install --remove-profile desktop`** deletes the profile's packages
  (AriFS v2 `delete`, ADR 0072) and its record.
- **Offline machines.** `cargo xtask usb-image --profile desktop` puts the
  packages in the stick's `pkgs/` and the profile in the installer's
  queue.
  - This needs a stick with AriFS v2, so 0.18 or later.
  - An older stick is refused with a message to write a new one.

### 3. Each profile package declares the system it needs

- **A package manifest may carry `requires = N`:** the oldest system
  version it runs on.
  - The installer refuses a package that needs a newer system, and says
    which one.
  - The updater installs the system first, then the packages, after the
    reboot.
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
| A | The profile mechanism: the `profile` index line; `install --profile` and `--remove-profile`; `profiles`; `upgrade` following profiles; `requires`; `usb-image --profile`; `gui_demo` as a test package. Scenarios with test packages. |
| B | `session = desktop` in init, and `launch` in the broker with its trust rule (audited: it touches the broker's grants). |
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
