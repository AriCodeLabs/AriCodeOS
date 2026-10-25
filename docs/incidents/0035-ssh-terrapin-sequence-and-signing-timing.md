# Incident 0035: SSH server open to Terrapin with old clients, no bound on sequence numbers, variable-time signing

- Opened: 2026-10-25. Status: fixed on `fix/ssh-protocol-hardening`.
- Found by: the failure-pattern audit of 2026-10-25 (audit D: M2, L6, L7;
  audit C: L8, L10; audit E: L2, I5). Not seen on a machine.
- Environment: sshd and `aricode-ssh`, `aricode-crypto` Ed25519.

## Evidence

- Strict key exchange (D-L7): sshd announced `kex-strict-s` but accepted
  clients that did not offer `kex-strict-c`. With
  `chacha20-poly1305@openssh.com`, the only cipher, such a client is open
  to Terrapin (CVE-2023-48795): a man in the middle deletes packets at the
  start of the encrypted stream.
- Sequence numbers (C-L10, D-L6): the ChaCha20 nonce is the packet
  sequence number and keys are never exchanged again; the counters wrapped
  silently after 2^32 packets, reusing nonces.
- Ed25519 signing (D-M2): the host key signs every key exchange; the point
  multiplication branched on the secret scalar's bits and the reduction
  modulo L on the nonce's, a timing side channel for a network observer.
- Channels (C-L8): `shell`/`exec` were accepted on a channel the server
  had already closed, and several times on one channel.
- Logs (E-L2): the user name of an authentication attempt, sent before
  authentication, was printed raw: newlines forged log lines, escapes
  reached the terminal. The failure limit allowed 11 failures (E-I5).

## Fix

- A client must offer strict key exchange; others are refused with the
  reason. OpenSSH 9.6 and PuTTY 0.80 and later offer it.
- A connection ends before either counter reaches 2^31 packets.
- `ed25519::public_key` and `sign` use a constant-time ladder
  (`mul_secret`, masked selection with `Fe::cswap`) and a branchless
  reduction modulo L; verification keeps the variable-time path, which only
  sees public values.
- One `shell` or `exec` per channel, never on a closed one.
- `aricode_ssh::log_safe` prints user names as printable ASCII, at most 32
  bytes; the tenth failure disconnects.
- Tests: `strict_key_exchange_is_required`, `user_names_are_made_log_safe`,
  `constant_time_paths_agree`, and the OpenSSH client tests.
