# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
from pathlib import Path
import shutil, hashlib, subprocess, json
root=Path.cwd(); out=root/'build/review-early-confirm'; out.mkdir(parents=True,exist_ok=True)
disk=out/'disk.img'
shutil.copy2(root/'build/scenarios/crypto.random/disk.img',disk)
with disk.open('r+b') as f:
    f.seek(6144*512);a=f.read(24576*512);f.seek(30720*512);f.write(a)
    state=bytearray(512);state[:8]=b'ARIBOOT\0';state[8:12]=bytes([1,0,1,3]);state[60:64]=hashlib.sha256(state[:60]).digest()[:4]
    f.seek(79872*512);f.write(state)
shutil.copytree(root/'build/esp',out/'esp',dirs_exist_ok=True)
(out/'esp/aricodeos/cmdline.txt').write_text('qemu_exit')
for attempt in [1]:
    vars=out/f'vars-{attempt}.fd';shutil.copy2('/usr/share/OVMF/OVMF_VARS_4M.fd',vars)
    cmd=['qemu-system-x86_64','-machine','q35','-cpu','qemu64','-m','512M','-no-reboot','-display','none','-monitor','none','-serial','stdio','-netdev','user,id=net0,restrict=on','-device','virtio-net-pci,netdev=net0,disable-legacy=on,disable-modern=off','-drive',f'if=none,id=disk0,format=raw,file={disk}','-device','virtio-blk-pci,drive=disk0,disable-legacy=on,disable-modern=off','-drive','if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd','-drive',f'if=pflash,format=raw,file={vars}','-drive',f'format=raw,file=fat:rw:{out/"esp"}','-device','isa-debug-exit,iobase=0xf4,iosize=0x04']
    r=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=40)
    (out/f'boot-{attempt}.log').write_bytes(r.stdout)
    print('boot',attempt,'exit',r.returncode)
    for line in r.stdout.decode(errors='replace').splitlines():
        if 'slots:' in line or 'confirmado' in line or 'net_client' in line or 'init' in line and ('salio' in line or 'terminado' in line): print(line)

with disk.open("rb") as f:
    f.seek(79872*512); state=f.read(64)
print("State after failed init: active=",state[9],"pending=",state[10],"tries=",state[11])
