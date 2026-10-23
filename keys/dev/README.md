# Development keys

Ed25519 key pair **for development and QEMU tests only**. The seed
(`ed25519.seed`) is in the repository on purpose: anyone can sign images
that AriBoot will accept in the test environment. A real system uses a key
pair generated outside the repository (`cargo xtask keygen DIR`) whose seed
is never versioned; `--signing-key DIR` on `usb-image`, `publish-update` and
`serve-update` signs with it and builds every verifier with its public key
(ADR 0029, "The signing key").

- `ed25519.seed`: private seed (32 bytes, hex). Do not use outside tests.
- `ed25519.pub`: public key (32 bytes, hex).
- `dev-channel/`: the test pair of the development update channel (ADR
  0045), seed public as well. Test builds sign the stable channel with the
  pair above and the development channel with this one; a build with a
  real `--signing-key` never trusts either of them (`aricode_crypto::trust`
  refuses to compile, xtask refuses to select them), and its development
  channel is `keys/dev-channel/`.
