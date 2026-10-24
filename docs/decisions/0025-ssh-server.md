# ADR 0025: SSH server with session shells launched by init

- **Date:** 2026-10-11
- **Status:** Accepted

## Context

Working with the reference laptop meant photos of the screen and moving the
USB stick back and forth (`diag` + `fs-cat`). The system already had a TCP
stack with a server side, ChaCha20, SHA-256/512, Ed25519 and a shell. Remote
access had to be encrypted and authenticated: a plain telnet-like console
would open an unauthenticated shell on the user's network.

## Decision

1. **Cryptography** (`aricode-crypto`): a shared GF(2^255-19) module,
   X25519 with a constant-time Montgomery ladder (RFC 7748 vectors),
   Poly1305 (RFC 8439) and `chacha20-poly1305@openssh.com`, checked against
   an independent computation with the OpenSSL command line.
2. **Protocol as a library** (`aricode-ssh`): an SSH-2 server state machine
   with no I/O (bytes and events in, bytes out). Exactly one algorithm per
   slot: `curve25519-sha256` with strict KEX (Terrapin mitigation),
   `ssh-ed25519` host key, `chacha20-poly1305@openssh.com`, no compression,
   `publickey` authentication with `ssh-ed25519` keys, one `session`
   channel (`pty-req`, `shell`, `exec`, `env`, `window-change`, flow control,
   `exit-status`). Host integration tests drive it with the real OpenSSH
   client (exec with exit status, interactive pty session, refused key).
3. **`sshd` service**: listens on TCP port 22 through the network service,
   one connection at a time. The host key seed lives in AriFS
   (`ssh_host_ed25519_key`, created from the kernel RNG on first start; its
   fingerprint is logged); logins are checked against `authorized_keys`
   (OpenSSH format), reread on every connection. Since 2026-10-17
   (incident 0001 follow-up): while the file service does not answer, the
   last keys stay in use and the reread is tried at most once a minute,
   logged once; a file the service reports missing empties the keys.
   Consequence: a key removed from `authorized_keys` during a file service
   outage stays valid until the service answers again. With no keys left,
   a new connection rereads the file before being refused, so a key
   installed later counts at once. A host key file that cannot be read is
   never overwritten: that boot uses a key of its own. Session endpoints
   are pairs (ADR 0030): sshd holds only the send side of the input and
   the receive side of the output, so a session shell that crashes ends
   the SSH session with exit status 139 (`ssh.shell_fault`) instead of
   leaving it open until the client disconnects.
4. **Session shells launched by init** (`aricode-proto::session`): sshd has
   no SPAWNER. For `shell`/`exec` it sends init a request with two fresh
   endpoints; init launches `arish` in session mode (`SHELL_SESSION`, and
   `SHELL_QUIET` for exec: no banner, prompt or echo) with the session
   endpoints in place of the console. Raw bytes travel in IPC messages; an
   empty message is end of stream in either direction. With a pty, sshd
   turns `\n` into `\r\n`.
5. **Images and tests**: `cargo xtask usb-image --authorized-key KEY.pub`
   installs a public key; QEMU forwards a host port to the guest's 22 (17022
   for `cargo xtask run`, one chosen per run in the scenarios since ADR 0040),
   and scenario key `[host_ssh]` generates a client key, installs it, and
   runs commands with the host's `ssh` (`ssh.exec`: ls, cat, write and
   shutdown over SSH).

## Security notes (2026-10-11)

- **Closed by default.** Without an `authorized_keys` file holding at least
  one `ssh-ed25519` key, sshd does not open port 22 at all. There is no
  password authentication and no default key: the only way in is a key the
  owner installed (`cargo xtask usb-image --authorized-key`, or `write
  authorized_keys ...` from the console). Removing the file closes the door
  for new connections.
- **Nothing secret in the repository.** The repository holds no SSH keys.
  Test clients get a fresh key pair per run (`ssh-keygen` into the scenario
  directory), never committed. `keys/dev` holds only the documented image
  signing key for QEMU tests, unrelated to SSH.
- **Entropy.** CPUs without `rdrand` gave the kernel generator little more
  than the cycle counter. Every device, keyboard and serial interrupt now
  stirs the cycle counter into a pool mixed into each read; sshd also
  carries a seed across boots (`random_seed` in AriFS, replaced before
  randomness is drawn for a new host key or for the first accepted
  connection of each boot, retried on the next connection if the save
  fails; a boot nobody connects to does not write it, incident 0005) and the host key for a USB image is generated on the build host
  from `/dev/urandom` (`build/ssh_host_ed25519_key`, fingerprint printed at
  build time so it can be checked against the first `ssh` prompt).
  Since 2026-10-22 (ADR 0039) sshd asks the kernel what seeded its
  generator (`system_info(RANDOM_SOURCE)`) before it draws a host key:
  when the source is not a hardware one (or the kernel does not say) and
  no `random_seed` from an earlier boot was read, it records the fault
  event `ssh entropy weak` (code = the `EntropySource` number, 2 for TSC +
  interrupts, 0 when unknown) and says on the console that the new host
  key is weak and that `cargo xtask usb-image --authorized-key` installs
  one made on the build host. It does not refuse to start, which would
  lock the owner out; the key is drawn as before, from the kernel
  generator and sshd's own state, with no earlier boot's seed behind it in
  exactly this case (with one, it is mixed in and nothing is reported). `diag` shows the source in its `entropy`
  line (`ssh.entropy_weak`, `crypto.entropy_rdrand`).
  Since ADR 0043 the kernel generator itself carries a seed across boots
  (AriBoot hands it over from the `ARIBOOT` partition): a generator with
  it mixed in counts as strong, so `ssh entropy weak` is recorded only
  when neither that nor sshd's own `random_seed` came from an earlier
  boot, and the weak line names why (`no carried seed (first boot)`).
  sshd keeps its `random_seed`: it also serves a disk without `ARIBOOT`.
- **Audit trail.** Every refused key is logged with the user name and the
  peer address; after ten failures the connection is dropped.
- **No monopoly by idle clients** (audit of 8910d1f, finding 2). A
  connection that has not authenticated within 30 seconds is dropped with a
  DISCONNECT and logged, so a client that opens TCP and sends nothing cannot
  hold the single served connection. Scenario `ssh.exec` keeps such a client
  open while the real commands run. Authenticated sessions have no idle
  limit; the owner closes them.
- **Output is never cut by the close** (audit finding 3). `Server::exit`
  defers the exit status, EOF and close until every byte handed to
  `send_data` has left; with a client window smaller than the output, the
  close waits for the window adjustments. Regression test with 3 MiB
  through the OpenSSH client.
- **Who can read the files.** `authorized_keys`, the host key and the seed
  live in the AriFS root. Programs launched through the broker see only
  their private directory; the console shell, sshd and an authenticated
  SSH session can read and change them (an authenticated user already has
  the shell).
- **Exposure.** The laptop sits on the local network behind the router;
  sshd is only reachable from the Internet if a port is forwarded to it.
  The protocol parser runs before authentication, so the surface is the
  SSH library itself; it only accepts one algorithm per slot and bounded
  packet sizes.

## Consequences

- Remote shells have the same capabilities as the console shell (files,
  network, launching programs through the broker, power). Output of
  programs started with `run` still goes to the console, not to the session.
- One SSH connection at a time; no port forwarding, agent forwarding, SFTP,
  password authentication, re-keying or other algorithms.
- Ed25519 signing is not constant time; the signing key is the device's
  own host key, used once per connection.
- HTTPS (TLS 1.3 client) can reuse X25519, ChaCha20-Poly1305 and SHA-256;
  it still needs HKDF, the TLS record and handshake layers, and certificate
  verification (ECDSA/RSA and a set of trusted roots).
