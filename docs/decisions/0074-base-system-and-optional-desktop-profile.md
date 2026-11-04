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
- **Offline machines (phase B).** `cargo xtask usb-image --app DIR:VER
  --profile desktop:APP,...` installs the profile's apps on the stick and
  follows the profile (see "Phase B as built").
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
- **Each refused package is downloaded only once.** The index does not
  carry `requires`, so the updater remembers each package it refused, in
  `updater/refused`: name, version, the SHA-256 the index gave, and the
  level required.
  - `update` holds back that exact package and says why, and `upgrade`
    does not fetch it again.
  - Other bytes or another version are fetched normally.
  - A line stops holding anything back once the system offers the level,
    and is pruned.
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
| B (done) | `session = desktop` in init; `launch` in the broker with its trust rule (audited: it touches the broker's grants); `gui_demo` as a test package; `usb-image --profile`. Scenarios `apps.launch`, `apps.desktop_session` and `usb.image_desktop`. |
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
  - L10: new tests, including `apps.profile_not_reported`.
  - L4: a refused package is remembered and not downloaded again
    (`updater/refused`). The scenarios check it with a new expectation,
    `serial_once`.
  - `cargo test -p aricode-pkg` builds without `--features write`.

## Phase B as built (2026-11-03)

- **`gui_demo` is a test package** (`examples/gui_demo`). With `gui` init
  starts only the compositor; the GUI scenarios install the demo and start
  it from the shell.
- **The kernel lends the screen to whoever maps it.** The design assumed
  init could start the compositor at any time. Without `gui` on the
  command line, though, the kernel console draws on the same screen and
  every log line would scroll over the windows. So mapping the framebuffer
  now lends the screen to the mapper, and the console takes it back when
  the last mapping goes: unmapped, or with its process. The capability to
  map it stays the right to hold it. init hands it only to the
  compositor; a display driver that keeps the display's BAR covering the
  framebuffer (ADR 0051) holds the screen the same way when it maps it.
- **The console's keyboard follows its screen.** On a laptop, the kernel
  reads the internal PS/2 keyboard for the console. With the desktop on the
  screen, those keys would have reached a hidden shell. While any process
  holds the screen, PS/2 characters are not given to the console, and the
  kernel says how many it withheld. This also closes the same leak under
  `gui`.
- **The desktop needs a USB or virtio keyboard.** The compositor reads the
  input services, and the PS/2 keyboard is not one of them yet. Each input
  service now says in its `Info` reply whether it has a keyboard, a pointer
  or both (`input::KIND_*`), and init starts the desktop only with a
  keyboard among them. Without one, init boots into the text console and
  says why. Giving the compositor the PS/2 keyboard and touchpad was
  future work at the time; ADR 0075 did it (2026-11-04), and `ps2_input`
  now counts as a keyboard.
- **`launch` is an endpoint of its own.** It is a send handle to a second
  broker endpoint, so every request there comes from a `launch` holder.
  - The broker grants it only to a `launcher` that is installed and
    signed, never to an initrd program.
  - There the broker serves only background launches of installed
    applications, without a console, each with its own manifest.
  - It refuses, and says so: an initrd program, the launcher itself, a
    launch that waits for the exit or carries a console, and service
    status.
- **The session.**
  - The shell's `session [shell|desktop]` writes `session`.
  - Following the `desktop` profile writes `session = desktop`, and leaving
    it writes `session = shell`.
  - init starts the desktop only when all of these hold: the profile is
    followed, `apps/launcher.aripkg` exists, there is a display, and there
    is a USB or virtio input. Otherwise it says which one is missing. A
    damaged file means the text console.
  - The shell keeps running on COM1 and over SSH.
- **The way back to the text console** is always at hand, on any keyboard.
  - **Ctrl+Alt+Backspace** on a keyboard the compositor reads makes it
    leave.
  - The same chord on the PS/2 keyboard, the only key the kernel acts on
    while the screen is held, makes the kernel stop every process holding
    the screen.
  - The session also ends when its launcher exits; phase C's launcher
    offers that as a menu entry.
  - Whichever way it ends, init:
    1. stops the launcher, the apps it started and the compositor;
    2. the kernel gives the console its screen and PS/2 keyboard back;
    3. init starts the console bridge over the input services, unless the
       compositor did not end within 5 s, which init says.
  - `session shell` then makes it last across boots.
- **`gui` on the command line** keeps its compositor for the whole boot. No
  desktop session is started on it, so a kiosk or test boot never loses
  its screen to a launcher's exit.
- **`usb-image --app DIR:VER --profile NAME:APP,APP`** (both repeatable)
  installs the apps in the stick's `apps/` directly, not in `pkgs/`, so
  the first boot already has them.
  - It records each profile with its apps as installed for it, and writes
    `session = desktop` for the desktop profile.
  - A fresh stick has nothing to replace, and the broker verifies each
    package at every launch, so the installer's version checks have
    nothing to decide.
  - A profile that names an app missing from the stick is refused, and so
    is a desktop profile without `launcher`.
  - Since the stick is always AriFS v2, there is no older stick to refuse.
- **Remaining limit, to decide in phase C:** the launcher's apps share the
  broker's four running slots with the shell's `run` and `spawn`.

## Audit of phase B (2026-11-04)

An independent read-only audit found that `launch` gives nothing beyond
the launched app's own manifest, and that the kernel lease is free of
lock-order and interrupt problems. Everything it found was fixed in the
same branch:

- **H1:** with the desktop up there was no local way back: PS/2 keys were
  withheld, the other keyboards belonged to the compositor, and
  `session = desktop` survived a power cycle. Ctrl+Alt+Backspace now
  works on every keyboard (`aricode_input::EscapeChord`, host tests;
  `usb.image_desktop_escape`, `gui.ps2_withheld`).
- **M1:** init did not notice the compositor dying. Its exit now ends the
  session too. This also showed that init kept the receive side of the
  compositor's endpoint, so every request to a dead compositor waited out
  its timeout. init now keeps only the send side (ADR 0030).
- **M2:** a mouse alone started the desktop. A keyboard is now required
  (`usb.image_desktop_no_keyboard`).
- **Low findings:**
  - L1: the kernel decides "screen" once, in `map_foreign`, at page
    granularity.
  - L2: a profile list that cannot be read is said as such.
  - L3: the inputs stay with a compositor that does not end.
  - L4: the launcher's apps are stopped with the session.
  - L5: `gui` keeps its compositor.
  - L6: `run_gui` closes what it made on every failure.
  - L7: the wording above.
- **Also:** a stopped process's log line now says who stopped it (another
  process's request, or Ctrl+Alt+Backspace) instead of always "the
  supervisor".

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

## Phase C as built

### The launcher (2026-11-04)

- **The owner chose the launcher's own pool** of six applications, apart
  from the shell's four (`Pool::Launcher` with one slot, `Pool::Apps` with
  six). A launcher started from the shell (`run launcher`, in the tests)
  takes a shell slot, but its applications still go to `Pool::Apps`;
  `stop_desktop_apps` stops them all when a desktop session ends.
- **The pool is not the only limit:**
  - The compositor has eight sessions and eight windows, at most two
    per session. One launcher, six apps and one graphical program from
    the shell fill the eight sessions. Applications with two windows
    each can fill the windows first: a later program then gets its
    session, and its window is refused as busy.
  - fsd and the network stack serve 15 sessions each (ADR 0036, 0037).
    With every slot taken by apps that use files or the network, a
    launch can find that table full. It then fails visibly, as those
    ADRs already allow.
- **The launcher asks the broker what to offer** (`Request::LIST`, an
  `Entry` per application). At index 0 the broker lists `apps/*.aripkg`
  again, at most every 5 s; a younger list is answered as it is. It keeps
  the packages that:
  - have a signed manifest and an application manifest that matches its
    signed hash (`aricode_pkg::inspect_streaming`, a few KiB per package;
    the program is not hashed, since the launch verifies the whole
    package);
  - run on this system;
  - open a window;
  - ask for nothing an application may not have.

  It says why it leaves each other one out. The launcher gets names,
  never files: it still holds only `window` and `launch`. The list is
  advice, but the rule behind it is enforced: the broker starts for the
  launcher only applications that open a window.
- **Flags:** the shell's endpoint refuses any flag but `DETACH` and
  `CONSOLE`. The launcher's endpoint takes `LIST` alone, or `DETACH`
  alone.
- **`desktop/launcher`** is a package of the desktop profile, in a new
  `desktop/` directory for the profile's packages. It shows a list, with
  Open, Refresh and "Leave the desktop" buttons, and the outcome of the
  last launch. Enter or a click opens an application; leaving ends the
  session.
- **The widgets the applications share** are in `aricode_gfx::widgets`
  (host-tested):
  - `Button`, `List` and `TextField`; the text field holds printable
    ASCII only, so no cut splits a character;
  - keys from `aricode_input::term::KeyDecoder`. It turns the bytes a
    window receives back into keys, since the compositor sends the
    editing keys as terminal escape sequences. The shell's line editor
    now uses the same decoder. A lone `Esc` is known only when the next
    byte shows it starts no sequence, so it arrives with that key.
- **The program just opened takes the focus.** Before, only a click gave
  a window the focus. Now a new window takes it when all of these hold:
  - it is the first window of its session;
  - the session was made at most 5 s earlier;
  - no key was pressed since (`aricode_gfx::new_window_takes_focus`).

  Any other window waits for a click. A program that opens a window later
  cannot take the keys typed in another, a password in a terminal among
  them.
- **Scenarios:**
  - `usb.desktop_launcher`: the list holds gui_demo, but not hello_ext
    (no window) nor a hello_more with a tampered signature. The first
    application opens from the keyboard, and a click leaves the desktop;
  - `apps.launch`: the pool answers `Limit` after six, hello_ext is
    refused for opening no window, and a list with another flag is
    invalid;
  - the GUI scenarios now expect the first window to lose the focus when
    the second opens.

### Audit of the launcher (2026-11-04)

An independent read-only audit found no high findings. Bounds, handles,
names and the launch rules all checked out. It found these, all fixed in
the same branch:

- **M1:** each list re-verified every package whole inside init, with no
  limit. Now it reads only the manifests, which the release key signed,
  and caches the list for 5 s.
- **M2:** any program could open a window at any time and take the
  focus. Now only a program just opened, before any key is typed, gets
  it.
- **M3:** the text field could split a UTF-8 character and panic. It now
  holds printable ASCII only.
- **M4:** a build directory under `desktop/` was not ignored.
- **L1:** the application manifest was read outside the verified hash.
  It is now checked against the signed hash.
- **L2:** the launcher could start applications without a window. The
  broker now refuses them.
- **L3:** unknown flags are refused on both endpoints.
- **L4 and L5:** the limits and the shared list are described above.
- **L6:** one key decoder now serves the shell and the windows. It
  handles parameters and an `Esc` inside a sequence. The unreachable
  PageUp and PageDown branches are gone.
- **L7:** a single click no longer opens an application.
- **L8:** new tests cover a tampered package left out of the list, a list
  with another flag, the exact pool count and the focus rule (host).
- **L9:** `stack-check` passes for init: 26 456 of 32 768 bytes.

### The terminal (2026-11-04)

- **A new capability, `terminal`.** Like `launch`, the broker grants it
  only to an installed, signed package named `terminal`
  (`Caps::TRUSTED_TERMINAL`), never to an initrd program. It is a send
  handle to an endpoint of its own, `terminals`, so a request there is
  known to come from the terminal.
- **Init serves it as it serves sshd** (`Sessions`, ADR 0025). The
  differences:
  - only interactive shells, never sshd's `Exec` or `Ready`;
  - at most four shells, apart from SSH's four.

  Each shell is the owner's own, the same as at the console or over
  SSH. The shell keeps its own file and network sessions, and nothing
  passes through the terminal's manifest, as the design said.
- **`desktop/terminal` only carries bytes:**
  - it sends the keys of its window to the shell;
  - it draws the shell's text with the console's own grid
    (`aricode-fb-console`), which already understands the line editor's
    output;
  - the grid (about 64 KiB) lives in a static, too large for the stack.
- **No polling.** `Window::start_poll` sends a poll without waiting for
  its answer, so the terminal waits on its shell's output and on its
  window's next event in one `ipc_receive_any`.
- **When the shell ends, the terminal closes.** Killing the terminal,
  for instance when the desktop ends, closes the shell's input, so the
  shell ends too.
- **Scenario `usb.desktop_terminal`:** the launcher offers the terminal,
  and Enter opens it. `exit` typed in its window ends the shell, and the
  terminal reports the text it showed (the banner, the prompt and the
  echo).

