# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
#
# Continuous test container for AriCodeOS (ADR 0040): the development
# image plus a script that keeps its own clone of the public repository in
# a named volume, runs `cargo xtask check`, `cargo xtask test --all` and
# `cargo xtask test --all --cpus 4` on each new commit of a branch
# (default develop) and writes a dated report to a mounted directory. It
# never touches a developer's checkout and holds no credentials: the
# repository is fetched over https. Build the development image first,
# both from the repository root:
#
#   docker build -t aricodeos-dev -f tools/docker/Dockerfile .
#   docker build -t aricodeos-ci -f tools/docker/ci/Dockerfile .
#
# Run (README.md, "Docker"; tests/README.md, "Continuous runs"):
#
#   docker run -d --name aricodeos-ci --restart unless-stopped \
#       --cpus 10 --memory 12g --memory-swap 12g --cpu-shares 512 \
#       -v aricodeos-ci-work:/work -v aricodeos-ci-cargo:/usr/local/cargo/registry \
#       -v "$HOME/aricodeos-ci-reports":/reports \
#       -v "$HOME/.cache/aricodeos-lock":/run/aricodeos-lock --user "$(id -u):$(id -g)" \
#       aricodeos-ci --loop
#
# The lock directory is the host's suite lock (ADR 0040): the test passes
# wait while another suite runs on the machine and fail without the mount.

ARG DEV_IMAGE=aricodeos-dev
FROM ${DEV_IMAGE}

COPY tools/docker/ci/ci.sh /usr/local/bin/aricodeos-ci

# The clone and its build cache live in /work (a named volume) and the
# reports go to /reports (a mounted directory). Both are writable by any
# uid so the container can run with the host user's (`--user`): a new
# named volume takes the permissions of its mount point in the image.
RUN mkdir -p /work /reports && chmod 1777 /work /reports

WORKDIR /work
ENTRYPOINT ["/usr/local/bin/aricodeos-entrypoint", "/usr/local/bin/aricodeos-ci"]
CMD []
