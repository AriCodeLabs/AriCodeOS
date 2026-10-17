# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
#
# Development and test environment for AriCodeOS: the same Debian 13
# packages, Rust toolchain and QEMU/OVMF the reference host uses, so
# `cargo xtask check` and `cargo xtask test --all` give the same result on
# any machine that runs Docker. The repository is NOT copied into the image:
# it is mounted at run time (see README.md, "Docker").
#
# QEMU runs without KVM (xtask never asks for it), so the image works on
# Linux, macOS and Windows hosts alike; a run is slower than on the host.
#
# Build:  docker build -t aricodeos-dev .
# Use:    docker run --rm -it -v "$PWD":/src -v aricodeos-cargo:/usr/local/cargo/registry \
#             --user "$(id -u):$(id -g)" aricodeos-dev cargo xtask test --all

FROM debian:trixie-slim

ARG RUST_VERSION=1.98.1

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    LANG=C.UTF-8

# Same list as README.md "Requirements", minus gdb, plus what the test
# runner calls: openssl (HTTPS scenarios), ssh and ssh-keygen (sshd
# scenarios), strip (binutils, system images), ca-certificates and curl
# (rustup only).
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        qemu-system-x86 ovmf nasm clang lld binutils git make \
        openssl openssh-client ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# The toolchain, components and targets from rust-toolchain.toml, plus the
# WebAssembly target of the example modules, installed at build time so a
# run needs no network for the compiler.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal \
            --default-toolchain "$RUST_VERSION" \
            --component rust-src,llvm-tools,rustfmt,clippy \
            --target x86_64-unknown-none,x86_64-unknown-uefi,wasm32-unknown-unknown \
    && chmod -R a+rwX "$RUSTUP_HOME" "$CARGO_HOME"

# No ports are exposed and no keys are baked in: scenarios that need a
# server start it inside the container on localhost only.
# The repository owner's uid is passed with --user so files written to the
# mounted checkout (build/, target/) belong to the host user.
WORKDIR /src
ENV HOME=/tmp
CMD ["cargo", "xtask", "test"]
