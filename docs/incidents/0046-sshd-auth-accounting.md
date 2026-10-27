# Incident 0046: sshd's authorized_keys and authentication accounting

- Opened: 2026-10-27. Status: fixed on `fix/sshd-auth-accounting`; the
  independent audit of the fix is pending.
- Found by: the failure-pattern audit of 2026-10-25, section "Info":
  authorized_keys lines with options are ignored silently; `none` and
  unsigned publickey queries are not counted as authentication failures.
  Not seen on a machine.
- Environment: `sshd` with an `authorized_keys` in AriFS; any SSH client
  that reaches port 22.

## Evidence

Host tests run on `develop` at `5ef2f6e`, before the fix (kept out of the
tree; the regressions below replace them):

1. `parse_authorized_keys` of `from="10.0.0.1",command="ls" ssh-ed25519
   <key> me@host` returned `[]`, and its type (`Vec<[u8; 32]>`) had no
   way to say that a line was dropped, or why: sshd printed nothing.
2. A `Server` in the authentication phase, fed 50 `none` requests and 50
   `publickey` requests without a signature for an authorized key,
   alternately: all 100 answered (`USERAUTH_FAILURE` 51, `USERAUTH_PK_OK`
   60) and the connection still open. `assertion failed: 100 requests
   answered: [51, 60, 51, 60, ...]`.

## Mechanism

1. `parse_authorized_keys` skipped every line whose first field was not
   `ssh-ed25519`. An OpenSSH line with options puts them first, so the
   line was skipped like a comment. Failing closed (the key never worked)
   but silent: an owner who restricted a key with `from=` or `command=`
   found it refused with nothing saying why. Lines of other key types
   (`ssh-rsa`, `ecdsa-sha2-*`) and keys that did not decode went the same
   way.
2. `Server::userauth` answered `none` (a methods query) and an unsigned
   `publickey` request for an authorized key (`PK_OK`) and returned before
   the failure counter, which only refused keys and other methods
   reached. A client could ask without bound within the 30 s
   authentication deadline, each answer costing a packet, and confirm
   again and again which of its public keys this machine accepts.

## Fix

1. **No options are supported, and that is said.** `parse_authorized_keys`
   returns `AuthorizedKeys`: the keys, and every line that is neither
   blank nor a `#` comment and gives no key, with its number and the
   reason (`KeyLineError::Options`, `KeyType`, `Malformed`; the first 16
   by number, all counted). sshd prints each one (`sshd: authorized_keys
   line 3 gives no key: it has options (from=, command=, restrict, ...),
   which are not supported`) and records `ssh key line refused` with the
   line number as its code, whenever what is refused changes (rereads of
   the same file after every connection, or every 5 s while closed, do
   not repeat it). With lines but no usable key it says `no usable key in
   authorized_keys: not listening`. Supporting options was not chosen:
   each one is policy sshd does not have (source addresses, forced
   commands, pty and forwarding rules), and taking the key without them
   would give it more than its line allows.
2. **Every authentication request counts but the first `none`.**
   `MAX_AUTH_ATTEMPTS` (10, the old failure limit) now counts refused
   keys and methods, key queries answered `PK_OK` and repeated `none`
   requests; a first `none`, the client asking for the methods (RFC 4252,
   5.2), is free, as in OpenSSH. The request that reaches the limit is
   answered with a DISCONNECT (reason 14, `too many authentication
   attempts`) and a new event, `TooManyAuthAttempts`, which sshd logs
   with the user name (made log-safe) and the peer. A usual OpenSSH login
   counts one request (the key query); one with several keys in the agent
   counts one per key tried.

## Regressions

- `aricode_ssh`: `lines_with_options_are_refused_by_number` (options
  first, `restrict`, a tab, other key types, a certificate, a line of
  words, blank and indented comment lines; the report bound),
  `authorized_keys_parsing` (now checks the refused lines too),
  `every_auth_request_but_the_first_none_counts` (repeated `none`,
  repeated queries, a refused key, another method, and a usual login
  that still succeeds); the OpenSSH client tests in
  `libs/ssh/tests/openssh.rs` (login, pty, refused key, large output)
  pass unchanged.
- QEMU: `ssh.key_options_refused` (a `restrict` line written from the
  console is said by number and recorded, sshd stays closed; a plain line
  opens port 22), and the `ssh` area scenarios.

## Open

- `usb-image --authorized-key` copies the key file as it is; a line with
  options is only reported once the stick boots.
