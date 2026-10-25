# ADR 0050: The file service's authority, closed on every path

- **Date:** 2026-10-25
- **Status:** Accepted (failure-pattern audit 2026-10-25: C-H1, E-M1, E-M2, C-M6, E-L3)

## Context

The file service (fsd) gives each program the authority of the session
its endpoint is bound to (ADR 0016, 0036): a private directory, one
read-only file, or the whole namespace. The 2026-10-25 audit found that
authority closed on the main path and open on others:

- a request whose session was retired while it was being served ran
  with no session, which meant the whole namespace (incident 0028);
- `read_files` accepted any name, so a signed manifest could share
  itself sshd's host key, `random_seed`, the updater's state or another
  program's private files (`hello_app/secret`), against ADR 0025's "programs
  launched through the broker see only their private directory";
- sshd, which parses unauthenticated network input, held fsd's main
  endpoint: every file, `Bind`/`Share`/`Attach`, and init's own
  descriptors (owner 0, never reclaimed). The updater had been denied
  `install` for the same reason (update-sources audit 1, M2);
- a program could be named `apps`, `pkgs` or `sshd`, and its private
  directory would then be the package store or sshd's files.

## Decision

1. **No session, no authority.** A request from a client endpoint whose
   session is gone is refused (`Denied`) before its operation is looked
   at; nothing is opened or staged for an owner that no longer exists.

2. **A shared file is a top-level file.** `Share` names a file without
   `/` (fsd refuses others: a file inside a directory is some program's
   private file). A manifest's `read_files` may name only such files,
   not starting with `.`, and none of `aricode_manifest::RESERVED_FILES`
   (`authorized_keys`, `ssh_host_ed25519_key`, `random_seed`). The
   manifest parser refuses the rest, so the build, the installer and the
   broker all refuse them.

3. **sshd has its own files.** Its host key and `random_seed` live in
   `sshd/`, a private-directory session (`Bind`) of each sshd generation.
   It reads `authorized_keys`, which stays in the AriFS root where the
   owner writes it (`write authorized_keys ...` and `usb-image
   --authorized-key` are unchanged), through a read-only session.
   `Share` with `len = SHARE_MAY_BE_MISSING` grants it even before the
   file exists, so sshd still opens port 22 once a key is installed. sshd
   no longer holds the main endpoint, and its descriptors die with it.

4. **Moving the files.** At boot, before launching sshd, init copies a
   root `ssh_host_ed25519_key` or `random_seed` into `sshd/` (unless a
   copy is already there), overwrites the root copy with zeros and
   empties it: AriFS cannot remove a file, and the root must not keep the
   host key. Each move is said on the console; a failure leaves the root
   copy and says so. The host fingerprint does not change.
   `cargo xtask usb-image` writes the host key into `sshd/` directly and
   creates it, and the image, with mode 0600 on the build host.

5. **Reserved program names.** `aricode_manifest::RESERVED_NAMES`
   (`apps`, `pkgs`, `sshd`) cannot be a manifest's `name`. `updater` stays
   allowed: it is the updater's own directory, and the initrd copy takes
   precedence over any installed package of that name.

## Consequences

- A compromised or malicious app cannot read sshd's host key, and a bug
  in sshd's pre-authentication parser no longer reaches
  `authorized_keys` for writing, the package store or the updater's
  state: sshd can change only its own two files.
- The console shell and SSH session shells keep the whole namespace (ADR
  0025: an authenticated user already has the shell), as do the
  installer and the updater through their sessions.
- Manifests that shared a file inside a directory stop parsing; none in
  the tree did (`notes_reader` shares `hola.txt`).
- Disk slack: the sectors of the emptied root copies are overwritten with
  zeros first; the journal keeps only the zeros.

## Tests

- `filesystem.retired_session`: a confined app that closes its session
  mid-request cannot open a root file (incident 0028).
- Host: `aricode-manifest` refuses system files, paths, dot names and the
  reserved names.
- `ssh.*` scenarios: sshd serves with its own sessions; the host key it
  uses is the one the image put in `sshd/`.
- `ssh.host_key_moved`: a disk whose root holds the host key (an older
  release's layout) boots with the same fingerprint, the key moved into
  `sshd/` and the root copy empty.
