# Incident 0055: ML-KEM's frames exceed sshd's stack budget

- Opened: 2026-10-31. Status: accepted with a bounded exception on
  `feature/ssh-pq-rekey` (ADR 0068); open until one of the ways out below.
- Found by: `cargo xtask check` (stack budget, incident 0018) once sshd
  linked `ml-kem` for the `mlkem768x25519-sha256` key exchange.

## Evidence

`cargo xtask stack-check` on sshd, against the default budget of half the
64 KiB stack for the deepest path and a quarter for one frame:

| Build | Deepest path | Largest frame |
|---|---|---|
| release, ML-KEM inlined in the packet loop | 39 304 | 26 512 (`Server::feed_packets`) |
| release, ML-KEM out of line | 31 352 | 24 352 (`mlkem_encapsulate`) |
| release, out of line, `ml-kem` `alloc` | 29 736 | 22 736 (`mlkem_encapsulate`) |
| dev, out of line, `ml-kem` `alloc` | 32 880 | 19 632 (`EncryptionKey::encrypt`) |
| release, isolated and scrubbed in `aricode-crypto` | 33 464 | 23 456 (`isolated`, ML-KEM inlined) |
| dev, isolated and scrubbed in `aricode-crypto` | 37 136 | 19 632 (`EncryptionKey::encrypt`) |

The last two rows are the state after the audit of ADR 0068 (L1). The
encapsulation runs in a frame of its own, and the 24 KiB below it are
then zeroed. The scrub's two 12 KiB frames reach no deeper than ML-KEM
itself; the isolating frame adds about 4 KiB to the dev path.

Measured natively by painting the stack (scratch harness), one ML-KEM-768
encapsulation with its key check uses:
- about 30 KB with `ml-kem` 0.3.3;
- about 27 KB with `libcrux-ml-kem` 0.0.11 portable, 30 KB with AVX2.

## Mechanism

FIPS 203 encryption works on a 3×3 matrix of polynomials and several
vectors of them. `ml-kem` keeps them in the frame of
`EncryptionKey::encrypt`, which belongs to another project. Its `alloc`
feature moves the encryption key to the heap, but not this working data.

## Decision

- The encapsulation runs in `aricode_crypto::mlkem::encapsulate`, out of
  line: its frames exist only during a key exchange, at the bottom of the
  packet loop, and never add to the loop's own frame.
- `ml-kem` is built with `alloc`.
- sshd gets an exception in `tools/xtask/src/stack.rs`, as close to the
  measurement as the check allows: 38 KiB for the path and 23 KiB for one
  frame. Any growth fails the check again.
- What stays: 26 KiB of the 64 KiB stack for what the analysis cannot see.
  User programs are built with inline stack probes, so a large frame
  cannot jump over the guard page. Crossing the stack kills sshd there; it
  does not corrupt memory.

## Known weaknesses of the exception (audit of ADR 0068, I1)

- It covers the whole program, not the ML-KEM functions: another sshd
  frame could grow up to 23 KiB without the check noticing. The check
  has no per-function budget yet.
- Painting the stack on the host measured about 30 KB for one
  encapsulation with its key check, more than the static figure below the
  same point. The static analysis may undercount here, which eats into
  the 26 KiB margin. Measuring sshd's real high-water mark in QEMU after
  an ML-KEM exchange would settle it.

## Ways out

- A per-program stack size in the kernel, so sshd gets a larger stack and
  the default budget again.
- An ML-KEM implementation that keeps its matrix on the heap, or upstream
  `ml-kem` doing so with `alloc`.
