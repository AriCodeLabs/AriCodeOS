# ADR 0027: System updates over the network

- **Date:** 2026-10-11
- **Status:** Accepted

## Context

Every change for the laptop meant writing the USB stick again. Phase 30
already had everything but the transport: signed system images in A/B slot
partitions, an `UPDATE` partition, a trusted `updater` that verifies and
installs, and a loader that tries the pending slot and rolls back. The
owner wanted to update from the development machine without touching the
stick, and asked whether a domain was needed.

## Decision

1. **Update channel = a directory over HTTP(S).** `cargo xtask
   publish-update [--version N]` writes `build/update/aricodeos-N.img` (the
   slot layout: `ARISLOT` header plus the signed container) and
   `manifest.txt` (`version`, `image`, `size`, `sha256`). `cargo xtask
   serve-update [--port 8080]` publishes and serves that directory on the
   LAN. Any static web server or a domain can serve the same two files
   later; nothing in the protocol depends on the host name.
2. **Security comes from the image signature, not from the transport.**
   The channel may be plain HTTP: the `updater` verifies the Ed25519
   signature of the manifest inside the image and the SHA-256 of kernel
   and initrd (ADR of Phase 30) before installing, exactly as for an image
   written by hand. The channel's `sha256` and `size` only detect a cut or
   corrupted download early. HTTPS (`https://` channel, ADR 0026) adds
   privacy and server authentication when the channel is public.
3. **`updater` gains the network.** Manifest `caps = system, fs, net,
   block`. Argument `1` (shell `update`): read the channel URL from
   `update_url` in its private directory, fetch `manifest.txt`, compare the
   channel version with the active slot's (read and verified from disk),
   and download the image straight into `UPDATE` sector by sector (the
   header sector is written last, so a cut download never leaves a
   plausible image), then run the Phase 30 install path. Argument `2`
   (`update check`) only reports both versions. Argument `0` (`update
   apply`) installs what `UPDATE` already holds.
4. **Shell.** `update http://host:port/path/` saves the channel URL (also
   `cargo xtask usb-image --update-url URL`) and runs the update; `update`
   reuses the saved URL; then `reboot`. The loader boots the pending slot,
   init confirms it, and a failed boot rolls back to the previous slot as
   before.
5. **Streaming HTTP.** `aricode-http::fetch_with` and
   `aricode-https::get_with` deliver the body to a sink piece by piece;
   the buffered `fetch`/`get` are built on them. The updater's sink
   hashes and writes sectors through the block client.
6. **Kernel: idempotent foreign mappings.** Running the updater twice in
   one boot exposed a latent bug: block servers cached client DMA buffers
   by physical address, but the kernel removes a freed buffer from every
   IOMMU domain, so a new client that got the same pages faulted
   (`iommu: DMA fault ... write to page without write permission`).
   `mmio_map` of a region a process already mapped now returns the existing
   mapping and re-establishes the IOMMU entry; servers call
   `block::map_client_buffer` on every request instead of caching.

## Consequences

- Scenario `update.network`: empty `UPDATE`, channel served by the test
  HTTP server, `update check`, `update`, `reboot`, boot from B confirmed,
  `update check` reports up to date.
- Downgrades are allowed when the channel version differs from the active
  one (the owner controls the channel); only an equal version is skipped.
- One image size limit: the slot partitions (12 MiB) and the updater's
  4 MiB verification buffer; the current image is about 2.3 MiB.
- Not included: automatic reboot after a successful install, periodic
  checks, delta updates, or application package channels (apps still go
  through `installer`).
