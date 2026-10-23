#!/usr/bin/env python3
"""ELF PT_LOAD footprint and retained runtime heap; not RSS or heap usage."""
from pathlib import Path
import struct, subprocess, sys, csv
root = Path(sys.argv[1])
names = ['init','virtio_blk','virtio_net','rtl_net','sshd','virtio_input','usb_input','usb_ehci','console_bridge','compositor','fsd','shell','hello_app','bench','installer','updater','wasm_runner']
out = csv.writer(sys.stdout)
out.writerow(['binary','elf_file_bytes','load_file_bytes','load_pages_bytes','zero_fill_bytes','runtime_arena_bytes','user_stack_bytes','kernel_stack_bytes'])
for name in names:
    path = root / name
    data = path.read_bytes()
    assert data[:6] == b'\x7fELF\x02\x01'
    phoff = struct.unpack_from('<Q', data, 32)[0]
    size, count = struct.unpack_from('<HH', data, 54)
    disk, zero, pages = 0, 0, set()
    for i in range(count):
        kind, flags, off, va, pa, filesz, memsz, align = struct.unpack_from('<IIQQQQQQ', data, phoff+i*size)
        if kind != 1: continue
        disk += filesz
        zero += memsz-filesz
        if memsz: pages.update(range(va//4096, (va+memsz+4095)//4096))
    symbols = subprocess.check_output(['nm','-S',str(path)],text=True)
    arena = sum(int(line.split()[1],16) for line in symbols.splitlines() if 'user_heap5ARENA' in line)
    out.writerow([name,len(data),disk,len(pages)*4096,zero,arena,65536,32768])
