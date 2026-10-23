use aricode_gfx::{Canvas, Desktop};
use aricode_sched::{RoundRobin, Tid};
use std::{hint::black_box, time::Instant};

fn ns_per_call(mut f: impl FnMut(), iterations: u32) -> u128 {
    for _ in 0..100 {
        f();
    }
    let mut batches = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..iterations {
            f();
        }
        batches.push(start.elapsed().as_nanos() / u128::from(iterations));
    }
    batches.sort();
    batches[3]
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--io-only") {
        io_counts();
        return;
    }
    println!("scheduler: host nanoseconds per operation, median of seven batches; no kernel locks/context switch");
    for n in [16, 64, 256, 1024, 4096] {
        let mut sched = RoundRobin::new(Tid(0), Tid(1));
        for _ in 2..n {
            let tid = sched.allocate_tid();
            sched.add(tid);
        }
        assert_eq!(sched.terminated().count(), 0);
        let scan = ns_per_call(
            || {
                black_box(black_box(&sched).terminated().count());
            },
            10_000,
        );
        let schedule = ns_per_call(
            || {
                black_box(sched.yield_current(0));
            },
            10_000,
        );
        let both = ns_per_call(
            || {
                black_box(sched.yield_current(0));
                black_box(black_box(&sched).terminated().count());
            },
            10_000,
        );
        println!(
            "threads={n} empty_reap_scan_ns={scan} yield_ns={schedule} yield_plus_scan_ns={both}"
        );
    }
    for (w, h) in [(1024, 768), (1920, 1080)] {
        let mut a = vec![0; w * h];
        let mut b = vec![0; w * h];
        let mut desktop = Desktop::new(w as u32, h as u32);
        desktop.compose(
            &mut Canvas::new(&mut a, w as u32, h as u32, w as u32).unwrap(),
            |_, _| {},
        );
        desktop.move_pointer(1, 0);
        desktop.compose(
            &mut Canvas::new(&mut b, w as u32, h as u32, w as u32).unwrap(),
            |_, _| {},
        );
        let changed = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        println!("desktop={w}x{h} one_pixel_cursor_move_changed_pixels={changed} current_flip_writes={} current_flip_bytes={}", w*h, w*h*4);
    }
    if let Some(path) = std::env::args().nth(1) {
        let data = std::fs::read(path).unwrap();
        let rd = aricode_initrd::Initrd::parse(&data).unwrap();
        let cache: std::collections::BTreeMap<_, _> = rd
            .entries()
            .map(|e| {
                let e = e.unwrap();
                (e.name, e.data)
            })
            .collect();
        println!("initrd files={} bytes={}", rd.len(), data.len());
        for name in ["init", "shell", "bench", "missing"] {
            let reparsed = ns_per_call(
                || {
                    black_box(
                        aricode_initrd::Initrd::parse(black_box(&data))
                            .unwrap()
                            .find(black_box(name)),
                    );
                },
                10_000,
            );
            let parsed = ns_per_call(
                || {
                    black_box(rd.find(black_box(name)));
                },
                10_000,
            );
            let indexed = ns_per_call(
                || {
                    black_box(cache.get(black_box(name)));
                },
                10_000,
            );
            assert_eq!(rd.find(name), cache.get(name).copied());
            println!("lookup={name} parse_then_find_ns={reparsed} retained_parse_find_ns={parsed} btree_lookup_ns={indexed}");
        }
    }
}

fn io_counts() {
    use aricode_arifs::{AriFs, BlockDevice, FsError, SECTOR};
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default, Debug)]
    struct Counts {
        reads: usize,
        writes: usize,
        read_bytes: usize,
        write_bytes: usize,
        flushes: usize,
    }
    struct Disk {
        bytes: Vec<u8>,
        counts: Rc<RefCell<Counts>>,
    }
    impl BlockDevice for Disk {
        fn read_sector(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> Result<(), FsError> {
            self.read_sectors(lba, out)
        }
        fn write_sector(&mut self, lba: u64, data: &[u8; SECTOR]) -> Result<(), FsError> {
            self.write_sectors(lba, data)
        }
        fn read_sectors(&mut self, lba: u64, out: &mut [u8]) -> Result<(), FsError> {
            let offset = lba as usize * SECTOR;
            out.copy_from_slice(&self.bytes[offset..offset + out.len()]);
            let mut c = self.counts.borrow_mut();
            c.reads += 1;
            c.read_bytes += out.len();
            Ok(())
        }
        fn write_sectors(&mut self, lba: u64, data: &[u8]) -> Result<(), FsError> {
            let offset = lba as usize * SECTOR;
            self.bytes[offset..offset + data.len()].copy_from_slice(data);
            let mut c = self.counts.borrow_mut();
            c.writes += 1;
            c.write_bytes += data.len();
            Ok(())
        }
        fn flush(&mut self) -> Result<(), FsError> {
            self.counts.borrow_mut().flushes += 1;
            Ok(())
        }
    }
    let counts = Rc::new(RefCell::new(Counts::default()));
    let disk = Disk {
        bytes: vec![0; 300 * SECTOR],
        counts: Rc::clone(&counts),
    };
    let mut fs = AriFs::format(disk, 300).unwrap();
    let fd = fs.create("audit").unwrap();
    fs.replace(fd, &[0x5a; 8192]).unwrap();
    *counts.borrow_mut() = Counts::default();
    let mut out = [0u8; 4096];
    assert_eq!(fs.read(fd, 0, &mut out).unwrap(), 4096);
    assert_eq!(out, [0x5a; 4096]);
    println!("aligned_read_4KiB: {:?}", counts.borrow());
    *counts.borrow_mut() = Counts::default();
    assert_eq!(fs.read(fd, 1, &mut out).unwrap(), 4096);
    assert_eq!(out, [0x5a; 4096]);
    println!("unaligned_read_4KiB: {:?}", counts.borrow());
    *counts.borrow_mut() = Counts::default();
    fs.replace(fd, &[0xa5; 4096]).unwrap();
    println!("atomic_replace_4KiB: {:?}", counts.borrow());
    assert_eq!(fs.read(fd, 0, &mut out).unwrap(), 4096);
    assert_eq!(out, [0xa5; 4096]);
}
