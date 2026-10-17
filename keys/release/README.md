# Release signing key

`ed25519.pub` is the **public** half of the key that signs AriCodeOS
releases, update indexes and application packages published by the
project since 2026-10-17 (the first release built with it is 0.11.0; the
`develop` image on ghcr.io is built with it since that date). The seed
is kept outside the repository, on the author's build host, and is never
committed.

```text
1d3e24f3c7c753a5bcb384041ace6481736decd86345eab71e2a6ff410ad446e
```

Images and updates signed with this key are accepted only by systems
built with it (`cargo xtask usb-image --signing-key DIR`). The
development key in `keys/dev/` signs tests and QEMU scenarios only; its
seed is public, so nothing built with it should be upgraded over an
untrusted network (ADR 0029, "The signing key"). Release 0.10.0 predates
this key and was built with the development one.

Verify a published file against this key with
`cargo xtask verify keys/release/ed25519.pub FILE`.
