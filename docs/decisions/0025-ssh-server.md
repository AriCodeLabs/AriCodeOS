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
   (OpenSSH format), reread on every connection.
4. **Session shells launched by init** (`aricode-proto::session`): sshd has
   no SPAWNER. For `shell`/`exec` it sends init a request with two fresh
   endpoints; init launches `arish` in session mode (`SHELL_SESSION`, and
   `SHELL_QUIET` for exec: no banner, prompt or echo) with the session
   endpoints in place of the console. Raw bytes travel in IPC messages; an
   empty message is end of stream in either direction. With a pty, sshd
   turns `\n` into `\r\n`.
5. **Images and tests**: `cargo xtask usb-image --authorized-key KEY.pub`
   installs a public key; QEMU forwards host port 17022 to the guest's 22,
   and scenario key `[host_ssh]` generates a client key, installs it, and
   runs commands with the host's `ssh` (`ssh.exec`: ls, cat, write and
   shutdown over SSH).

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
