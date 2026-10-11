# Development keys

Ed25519 key pair **for development and QEMU tests only**. The seed
(`ed25519.seed`) is in the repository on purpose: anyone can sign images
that AriBoot will accept in the test environment. A real system uses a key
pair generated outside the repository (`cargo xtask keygen DIR`) whose seed
is never versioned; only the public key is built into the loader.

- `ed25519.seed`: private seed (32 bytes, hex). Do not use outside tests.
- `ed25519.pub`: public key (32 bytes, hex).
