# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
from pathlib import Path
import subprocess,time,selectors,json
base=Path('/var/tmp/aricode-probes-6ee19fb/qemu')
args=['qemu-system-x86_64','-machine','q35','-cpu','qemu64','-smp','1','-m','512M','-no-reboot','-device','intel-iommu,aw-bits=48','-drive',f'if=none,id=disk0,format=raw,file={base}/disk.img','-device','virtio-blk-pci,drive=disk0,disable-legacy=on,disable-modern=off,iommu_platform=on','-drive','if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd','-drive',f'if=pflash,format=raw,file={base}/OVMF_VARS.fd','-drive',f'format=raw,file=fat:rw:{base}/esp','-netdev','user,id=net0','-device','virtio-net-pci,netdev=net0,disable-legacy=on,disable-modern=off,iommu_platform=on','-device','virtio-keyboard-pci,id=kbd0,disable-legacy=on,disable-modern=off,iommu_platform=on','-device','virtio-mouse-pci,id=mouse0,disable-legacy=on,disable-modern=off,iommu_platform=on','-device','isa-debug-exit,iobase=0xf4,iosize=0x04','-display','none','-monitor','none','-serial','stdio']
(base/'command.json').write_text(json.dumps(args,indent=2))
commands=['run reviewa 1','run reviewb 2','run wasm_runner','run reviewa 3','run reviewb 4','shutdown']
p=subprocess.Popen(args,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
s=selectors.DefaultSelector();s.register(p.stdout,selectors.EVENT_READ)
data=b'';sent=0;deadline=time.monotonic()+100
try:
 while time.monotonic()<deadline:
  for key,_ in s.select(.2):
   chunk=key.fileobj.read1(65536)
   if chunk:data+=chunk
  if data.count(b'ari> ')>sent and sent<len(commands):
   p.stdin.write((commands[sent]+'\n').encode());p.stdin.flush();sent+=1
  if p.poll() is not None:break
 if p.poll() is None:p.kill()
 p.wait();data+=p.stdout.read()
finally:
 if p.poll() is None:p.kill();p.wait()
 (base/'probe-serial.log').write_bytes(data)
print('exit',p.returncode,'commands',sent)
for line in data.decode(errors='replace').splitlines():
 if any(x in line for x in ['review:','wasm_runner:','panic','salio','run review']):print(line)
