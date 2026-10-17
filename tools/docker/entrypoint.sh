#!/bin/sh
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
#
# Entry point of the development image. The container runs with the host
# user's uid (`--user`), which has no entry in /etc/passwd; OpenSSH refuses
# to run for a uid without a user ("No user exists for uid"), and the sshd
# host tests use it. nss_wrapper gives the current uid a temporary user and
# group, visible only to this process tree, without touching the image.
set -e
uid=$(id -u)
gid=$(id -g)
if ! getent passwd "$uid" >/dev/null 2>&1; then
    dir=$(mktemp -d /tmp/nss.XXXXXX)
    cp /etc/passwd "$dir/passwd"
    cp /etc/group "$dir/group"
    echo "ari:x:$uid:$gid:AriCodeOS developer:${HOME:-/tmp}:/bin/sh" >> "$dir/passwd"
    getent group "$gid" >/dev/null 2>&1 || echo "ari:x:$gid:" >> "$dir/group"
    # The library lives under the architecture's multiarch directory
    # (x86_64-linux-gnu, aarch64-linux-gnu, ...): find it instead of
    # assuming the host is x86-64.
    lib=$(find /usr/lib -name libnss_wrapper.so -print -quit)
    if [ -z "$lib" ]; then
        echo "aricodeos-entrypoint: libnss_wrapper.so not found; uid $uid has no user name and the sshd host tests would fail" >&2
        exit 1
    fi
    export NSS_WRAPPER_PASSWD="$dir/passwd" NSS_WRAPPER_GROUP="$dir/group"
    export LD_PRELOAD="$lib"
fi
exec "$@"
