# Development channel key

`ed25519.pub` is the **public** half of the key that signs the
development update channel (ADR 0045): builds of `develop` or a branch
published with `cargo xtask publish-update --channel dev` and their
indexes. The seed is kept outside the repository, on the author's build
host (`~/.config/aricodeos/dev-signing-key`, mode 600), and is never
committed. It was generated on 2026-10-23 with `cargo xtask keygen`.

```text
f831015cc017e95c40ac8f48034320f0b05ed7d77993221f6eaafaec8379c776
```

It is not the release key (`keys/release`) and never signs a release; the
release key never signs a development build. The key that verifies an
image or an index therefore also says its channel, and a machine moves
between channels only when its owner types `upgrade --channel ...`.

Every updater carries this public key, inert until the owner registers a
development source (`update add --channel dev URL`). AriBoot carries it
only on a machine installed from a stick built with
`cargo xtask usb-image --dev-source URL`: the development channel is
closed on every other machine.

Verify a development publication against it with
`cargo xtask verify keys/dev-channel/ed25519.pub build/update-dev/index build/update-dev/index.sig`.
