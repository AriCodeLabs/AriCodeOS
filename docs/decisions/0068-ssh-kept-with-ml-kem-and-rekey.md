# ADR 0068: SSH server kept, with ML-KEM, key re-exchange and a faster send path

- **Date:** 2026-10-31
- **Status:** Accepted. Step 5 of ADR 0063's order of work. Changes
  ADR 0025: one algorithm per slot, and no key re-exchange.

## Context

ADR 0063 asks for the comparison of the SSH server (`aricode-ssh` and
`services/sshd`, ADR 0025) with sunset and with Dropbear's design:
- **sunset** 0.6.0 (2026-08-02) is a Rust SSH client and server library by
  Dropbear's author. It is `no_std` without `alloc`, forbids `unsafe`,
  is under 0BSD and is on crates.io.
- **Dropbear** 2026.94 (2026-07-23) is the reference small SSH server, in
  C. It cannot be adopted (crates.io only, ADR 0063), so its design and
  its history are what is compared.

## Measurements

### Method

A scratch harness put each server library behind the same blocking TCP
loop on the host:
- this system's `aricode-ssh`;
- sunset 0.6.0, through its `Runner` API;
- the host's own OpenSSH 10.0 `sshd`, run unprivileged on loopback as the
  reference.

The client was OpenSSH 10.0 for all of them. The figures are:
- server CPU per connection, from `/proc/PID/schedstat`, over 100
  `ssh host true`;
- server CPU per GiB sent, over three 512 MiB outputs;
- whether a 2 GiB output arrives whole.

Running Dropbear and PuTTY, built from their sources, was refused by the
session's permission policy for code downloaded from outside. Dropbear
is therefore compared on its source and its changelog only, and PuTTY not
at all.

### Results (host, release builds)

| | `aricode-ssh` before | sunset 0.6.0 | OpenSSH 10.0 `sshd` | `aricode-ssh` after |
|---|---|---|---|---|
| Server CPU per connection | 0.40 ms | 0.50 ms (ML-KEM), 0.46 ms (X25519) | 17 ms (forks a session) | 0.46 ms (ML-KEM), 0.37 ms (X25519) |
| Server CPU per GiB sent | 1.36 s | 0.98 s (ML-KEM), 0.88 s (X25519) | 1.15 s | 0.86 s |
| 2 GiB output | cut at 1 GiB | whole | whole | whole |
| Key exchange OpenSSH 10 picks | curve25519-sha256 | mlkem768x25519-sha256 | mlkem768x25519-sha256 | mlkem768x25519-sha256 |

- The old server's 1 GiB cut is ADR 0025's limit: OpenSSH asks for new
  keys after about 1 GiB with this cipher, and the server refused.
- The old send path copied every byte four times and moved what was left
  of the pending output on each packet. sunset spent a third less CPU per
  byte.

### sunset as a server

sunset's server API cannot end a command:
1. It cannot send `exit-status`, so `ssh host command` always ends with
   status 255 and scripts cannot tell success from failure.
2. It cannot send EOF or close a channel. A server must end the TCP
   stream instead, and OpenSSH then exits without writing out what it
   still holds: in the harness, a 3 MiB output arrived as 1.3 MiB. The
   measurements above wait half a second before ending the stream, and
   the delay is subtracted.
3. It answers the client's EOF with its own (`handle_eof`). OpenSSH then
   takes no more output. `ssh host command </dev/null`, the usual form in
   scripts, got 2 MiB of a 512 MiB output. RFC 4254 5.3 makes EOF one
   direction only.

Adopting it would mean changing it. A fork cannot be used (crates.io
only), and the changes are upstream's to make. Its strengths, a
post-quantum key exchange and key re-exchange, are what this system's
server lacked.

### Dropbear's design and history

- Defaults: post-quantum hybrids `mlkem768x25519-sha256` and
  `sntrup761x25519-sha512`, new keys every 1 GiB or 8 hours, 10
  authentication attempts, 5 minutes to authenticate, up to 30
  unauthenticated connections and 5 per address.
- Its security fixes of 2025 and 2026 are in what this server does not
  have:
  - X11 and Unix socket forwarding;
  - `scp`;
  - forced commands and other `authorized_keys` options;
  - passwords and two-factor login.

  One is in what this change adds. In 2026.92, an authenticated peer could
  grow the server's memory without bound while a re-exchange the server
  started waited for it.

### ML-KEM implementations

The two serious Rust implementations, on the host. Both give the same
ciphertext and key from the same randomness.

| | One encapsulation with the key check | Stack used |
|---|---|---|
| RustCrypto `ml-kem` 0.3.3 | 28.6 µs | about 30 KB |
| `libcrux-ml-kem` 0.0.11, portable | 28.9 µs | about 27 KB |
| `libcrux-ml-kem` 0.0.11, AVX2 | 15.2 µs | about 30 KB |

libcrux is formally verified, and OpenSSH's own ML-KEM comes from it. It
is not taken, for three reasons:
- its `libcrux-platform` depends on `libc` on every target, which the
  dependency check refuses in user programs (ADR 0065);
- it is at version 0.0;
- its gain is 13 µs per connection, about 3% of the server's CPU for a
  handshake.

It is the better choice once `libcrux-platform` drops `libc` on x86-64.

## Decision

1. **The server stays this system's.** sunset's server cannot report an
   exit status, end a channel or keep sending after the client's EOF.
2. **`mlkem768x25519-sha256` first, `curve25519-sha256` kept** for
   clients older than OpenSSH 9.9, PuTTY 0.83 or Dropbear 2025.87.
   - ML-KEM-768 comes from RustCrypto's `ml-kem` 0.3.3 (Apache-2.0 or
     MIT), behind `aricode-crypto`'s `mlkem` feature, which the kernel and
     AriBoot leave off. The randomness comes from the server's own
     generator (`encapsulate_deterministic`, feature `hazmat`).
   - What `ml-kem` leaves on the stack is zeroed after each encapsulation:
     the shared key, and the randomness it can be recomputed from.
   - The client's encapsulation key must pass FIPS 203's checks.
   - K = SHA-256(ML-KEM key || X25519 secret), encoded as a string in the
     exchange hash and in key derivation, as in the draft and OpenSSH.
   - A recorded session stays secret against a future quantum computer.
     OpenSSH 10.1 and later warn about servers without such an exchange.
3. **Key re-exchange.**
   - The client may start one at any time after authentication. Before
     it, a re-exchange would only make the server compute, and is a
     protocol error.
   - The server starts one after 1 GiB or 2^28 packets either way under
     the same keys, as Dropbear and OpenSSH do with this cipher.
   - Strict KEX, agreed in the first exchange, restarts both sequence
     numbers at every NEWKEYS. A nonce never repeats under the same keys.
     A client that never completes a re-exchange is still disconnected
     at 2^31 packets.
   - Between its KEXINIT and its NEWKEYS the server sends only transport
     and key exchange messages (RFC 4253 7.1). Replies wait in a queue of
     at most 64; past that the client is disconnected, so the 2026.92
     growth cannot happen here.
   - Once the client has sent its KEXINIT, anything else before its
     NEWKEYS is a protocol error.
4. **The connection's generator forgets its key** once each exchange's
   secrets are drawn (`Rng::ratchet`, fast key erasure). Whoever reads
   sshd's memory later cannot recompute the earlier X25519 and ML-KEM
   secrets, so earlier key sets stay secret.
5. **Sending.** Channel output waits in a ring and goes straight into the
   packet, which is built and sealed in place in the output buffer.
6. **An exit waiting for its output** completes as soon as the output has
   left. Before, only a window adjust completed it, and a re-exchange
   could hold output back with the window open.
7. **Stack.** ML-KEM's encryption keeps its matrix on the stack, in
   frames of up to 23.5 KB. It runs out of line, only during a key
   exchange. sshd has a stack budget exception as tight as measured
   (incident 0055).

## Consequences

- OpenSSH 10 negotiates `mlkem768x25519-sha256` with the server inside
  AriCodeOS. Under QEMU TCG a connection plus `ls` takes 160–181 ms
  (median 181 ms) with it and 160 ms with X25519 only: launching the
  session shell dominates.
- Sessions no longer end after 1 GiB.
- sshd grows from 492 KiB to 524 KiB.
- Host CPU per byte sent drops by 37%, below sunset's and OpenSSH's.

## Limits

- **A close received during a re-exchange the server started** loses the
  server's own CHANNEL_CLOSE: it waits for the exchange, and sshd ends
  the connection on the close. The client, which is closing anyway, sees
  the connection end.
- **During the client's exchange only messages 1 to 4** are allowed
  besides KEX ones; RFC 4253 7.1 also allows the unassigned 8 to 19.
  Stricter than the RFC, and no client sends them.

- **Output through a session inside AriCodeOS** runs at 0.40 MB/s under
  QEMU TCG (`repeat 2000 help`, 3.3 MB). The protocol library is not what
  limits it:
  - each line the shell prints is an IPC message;
  - sshd waits up to 20 ms for the network between passes, and session
    output does not wake it.

  The same wait can delay the echo of a typed key. A single wait for the
  socket and the session endpoint (as ADR 0044 did for IPC, interrupts
  and deadlines) is future work.
- **One connection at a time.** Dropbear serves up to 30 unauthenticated
  ones. Here a client that connects again and again can hold the only
  slot for 30 seconds each time.
- **No time limit on keys**: Dropbear also renews them every 8 hours.
- **`sntrup761x25519-sha512`** (Dropbear, older OpenSSH) is not offered.
  ML-KEM is the standard one (FIPS 203), and clients without it fall back
  to X25519.

## Tests

- Host, against the OpenSSH client (`libs/ssh/tests/openssh.rs`):
  - each key exchange method;
  - a re-exchange every MiB started by the client (`RekeyLimit=1M`);
  - one started by the server after 1 GiB, over a 1.25 GiB output.
- Host unit tests (`libs/ssh/src/lib.rs`):
  - output and replies held during a re-exchange the server started, and
    released after its NEWKEYS, with the exit that waited for them;
  - the bound on held replies;
  - messages refused in the middle of a re-exchange the client started,
    and a re-exchange before authentication;
  - ML-KEM keys that fail the checks or have the wrong size;
  - the generator ratcheted by a key exchange;
  - random messages in every phase, and random packets through `feed`
    (key exchanges with keys of the right sizes included), never panic:
    20 000 and 200 rounds per run, 2 million and 200 000 once with
    `SSH_FUZZ_ITERATIONS`.
- Host, `aricode-crypto`: an ML-KEM encapsulation decapsulates to the same
  key, and leaves neither m, K nor r on the stack, in dev and release
  builds. Without the scrub the test fails (K found).
- The streamed outputs carry a position pattern, so data out of order is
  seen: sending the ring's two halves swapped fails at byte 4 161 447.
- QEMU: the `ssh` and `crypto` areas, 18 scenarios.

## Audit (2026-10-31)

An independent audit found no high or medium findings: no panic reachable
from the network, no nonce reuse, no unbounded memory, and no protocol
error. It checked:
- the hybrid exchange against the draft and OpenSSH;
- strict KEX and the sequence numbers at every NEWKEYS;
- every order of KEXINIT, ECDH_INIT and NEWKEYS;
- the bounds of the deferred queue and of the ring;
- the stack probes.

Applied:
- **L1.** ML-KEM's intermediates stayed on sshd's stack. They are now
  zeroed, and ML-KEM moved into `aricode-crypto` (also I5).
- **L2.** The generator kept one key for the whole connection. It now
  ratchets after each exchange.
- **L3.** The bulk tests sent uniform bytes. They now send a position
  pattern.
- **L4.** Some assertions could not fail:
  - the default exchange is now asserted from `ssh -Q kex`;
  - a second fuzz test goes through `feed` with real key sizes;
  - the wrong-size key is tested.
- **I4.** Re-exchanges before authentication are refused.

Recorded: I1 in incident 0055, and I2 and I3 under Limits.
