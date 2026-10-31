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

- The encapsulation runs in `mlkem_encapsulate`, marked `#[inline(never)]`:
  its frames exist only during a key exchange, at the bottom of the packet
  loop, and never add to the loop's own frame.
- `ml-kem` is built with `alloc`.
- sshd gets an exception in `tools/xtask/src/stack.rs`, as close to the
  measurement as the check allows: 34 KiB for the path and 23 KiB for one
  frame. Any growth fails the check again.
- What stays: 30 KiB of the 64 KiB stack for what the analysis cannot see.
  Crossing the stack kills sshd at the guard page; it does not corrupt
  memory.

## Ways out

- A per-program stack size in the kernel, so sshd gets a larger stack and
  the default budget again.
- An ML-KEM implementation that keeps its matrix on the heap, or upstream
  `ml-kem` doing so with `alloc`.
