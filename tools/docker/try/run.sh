#!/bin/sh
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
#
# Entry point of the try image: boots a fresh copy of the USB image, so
# every run starts from the released system. Extra arguments go to QEMU.
# ARICODEOS_CPUS sets the number of processors (default 2).
set -eu

work=$(mktemp -d)
cp /opt/aricodeos/aricodeos-usb.img "$work/disk.img"
cp /usr/share/OVMF/OVMF_VARS_4M.fd "$work/vars.fd"

cat <<EOF
AriCodeOS ${ARICODEOS_VERSION} in QEMU (no KVM: give it a few seconds).
AriCodeOS by Edwin F. Veliz Jaramillo (lynxcraft), licensed under Apache-2.0.
https://www.aricodeos.org/  https://github.com/AriCodeLabs/AriCodeOS

At the ari> prompt try: help, ps, mem, cpus, services, net, ls, diag,
ping 10.0.2.2, dns www.aricodeos.org, http https://www.aricodeos.org/
Quit with Ctrl-A X. Found something? Please open an issue on GitHub.

EOF

exec qemu-system-x86_64 \
    -machine q35 -cpu qemu64 -smp "${ARICODEOS_CPUS:-2}" -m 512M -no-reboot \
    -device intel-iommu,aw-bits=48 \
    -drive "if=none,id=disk0,format=raw,file=$work/disk.img" \
    -device virtio-blk-pci,drive=disk0,disable-legacy=on,disable-modern=off,iommu_platform=on \
    -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
    -drive "if=pflash,format=raw,file=$work/vars.fd" \
    -netdev user,id=net0 \
    -device virtio-net-pci,netdev=net0,disable-legacy=on,disable-modern=off,iommu_platform=on \
    -nographic \
    "$@"
