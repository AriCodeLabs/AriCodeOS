# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
#
# Try AriCodeOS without installing anything: the USB stick image, booted by
# QEMU + OVMF inside the container, with the serial console (and the `ari>`
# shell) on your terminal. Same QEMU machine as `cargo xtask run`: q35,
# qemu64, VT-d, virtio disk and network, no KVM, so it runs on any host
# with Docker. This image is for trying the system; development uses the
# image of the root Dockerfile.
#
# Build, from the repository root, after `cargo xtask usb-image`:
#   docker build -f tools/docker/try/Dockerfile --build-arg VERSION=0.10.0 -t aricodeos .
# Run (quit QEMU with Ctrl-A X):
#   docker run --rm -it aricodeos

FROM debian:trixie-slim

ARG VERSION=dev

RUN apt-get update \
    && apt-get install -y --no-install-recommends qemu-system-x86 ovmf \
    && rm -rf /var/lib/apt/lists/*

COPY build/aricodeos-usb.img /opt/aricodeos/aricodeos-usb.img
COPY LICENSE NOTICE /opt/aricodeos/
COPY tools/docker/try/run.sh /usr/local/bin/aricodeos

ENV ARICODEOS_VERSION=$VERSION

LABEL org.opencontainers.image.title="AriCodeOS" \
      org.opencontainers.image.description="AriCodeOS booted in QEMU: a capability-based microkernel operating system in Rust" \
      org.opencontainers.image.version="$VERSION" \
      org.opencontainers.image.authors="Edwin F. Veliz Jaramillo (lynxcraft)" \
      org.opencontainers.image.url="https://aricode.org" \
      org.opencontainers.image.source="https://github.com/AriCodeLabs/AriCodeOS" \
      org.opencontainers.image.licenses="Apache-2.0"

ENTRYPOINT ["/usr/local/bin/aricodeos"]
