from pathlib import Path
import shutil,struct
src=Path('/var/tmp/aricode-review-6ee19fb-uhBqbF/build/scenarios/boot.kernel_ready');dst=Path('/var/tmp/aricode-probes-6ee19fb/qemu-supervisor')
shutil.copytree(src,dst)
(dst/'esp/aricodeos/cmdline.txt').write_text('qemu_exit shell')
shutil.copyfile('/usr/share/OVMF/OVMF_VARS_4M.fd',dst/'OVMF_VARS.fd')
p=dst/'esp/aricodeos/initrd.img';b=p.read_bytes();count=struct.unpack_from('<H',b,6)[0];pos=8;files=[]
for _ in range(count):
 n=struct.unpack_from('<H',b,pos)[0];pos+=2;name=b[pos:pos+n];pos+=n;l=struct.unpack_from('<I',b,pos)[0];pos=(pos+4+7)&~7;data=b[pos:pos+l];pos=(pos+l+7)&~7;files.append((name,data))
hog=dict(files)[b'cpu_hog'];out=bytearray(b[:8])
for name,data in files:
 if name==b'virtio_input':data=hog
 out+=struct.pack('<H',len(name))+name+struct.pack('<I',len(data));out+=b'\0'*((-len(out))%8);out+=data;out+=b'\0'*((-len(out))%8)
p.write_bytes(out)
with (dst/'disk.img').open('r+b') as f:
 for sector in [6144,79872]:f.seek(sector*512);f.write(bytes(512))
s=Path('run-qemu.py').read_text().replace("/qemu')","/qemu-supervisor')")
s=s.replace("commands=['run reviewa 1','run reviewb 2','run wasm_runner','run reviewa 3','run reviewb 4','shutdown']","commands=['services --detail','ps','shutdown']")
s=s.replace("['review:','wasm_runner:','panic','salio','run review']","['virtio_input','cpu_hog','sin dispositivos','servicios de arranque','PID','estado','reclamado']")
Path('run-supervisor.py').write_text(s)
