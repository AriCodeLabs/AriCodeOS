// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel seed carried across boots (ADR 0043): reads the seed from
//! sectors 1 and 2 of the `ARIBOOT` partition, writes its successor to the
//! other copy and blanks the old one, and only if the successor reached
//! the disk hands the kernel its share in `BootInfo`. A seed whose
//! successor could not be written is not used: the next boot would find it
//! again. The loader writes no first seed (it has nothing better than the
//! cycle counter): init writes one from the kernel generator.

use crate::slots::Disk;
use aricode_ab::seed::{pick, plan, split, Record, Stored, COPIES};
use boot_protocol::carried_seed;
use log::{info, warn};

/// Cycle-counter samples hashed into the split, taken around the reads.
const NOISE_SAMPLES: usize = 8;

/// What goes into `BootInfo`: the status and, with `CARRIED`, the
/// kernel's share. Wiped when dropped.
pub struct Carried {
    pub status: u32,
    pub seed: [u8; 32],
}

impl Carried {
    pub const fn none(status: u32) -> Self {
        Self {
            status,
            seed: [0; 32],
        }
    }
}

impl Drop for Carried {
    fn drop(&mut self) {
        aricode_crypto::wipe(&mut self.seed);
    }
}

/// Cycle counter into noise sample `i`.
fn stamp(noise: &mut [u8; NOISE_SAMPLES * 8], i: usize) {
    // SAFETY: rdtsc has no side effects.
    let t = unsafe { core::arch::x86_64::_rdtsc() };
    noise[i * 8..i * 8 + 8].copy_from_slice(&t.to_le_bytes());
}

/// Takes the carried seed of the disk whose `ARIBOOT` partition starts at
/// `first_lba` and has `sectors` sectors.
pub fn carry(disk: &mut Disk, first_lba: u64, sectors: u64) -> Carried {
    if sectors <= COPIES[1] {
        warn!("seed: ARIBOOT has no room for the seed copies; no carried seed");
        return Carried::none(carried_seed::NO_STORE);
    }
    let mut noise = [0u8; NOISE_SAMPLES * 8];
    stamp(&mut noise, 0);
    let mut a = disk.read(first_lba + COPIES[0], 1);
    stamp(&mut noise, 1);
    let mut b = disk.read(first_lba + COPIES[1], 1);
    stamp(&mut noise, 2);
    let mut stored = pick([a.as_deref(), b.as_deref()]);
    for copy in [&mut a, &mut b].into_iter().flatten() {
        aricode_crypto::wipe(copy);
    }
    let p = plan(&stored);
    let mut current = match stored {
        Stored::Missing => {
            info!("seed: the seed store is blank (first boot): no carried seed");
            return Carried::none(carried_seed::MISSING);
        }
        Stored::Unreadable => {
            warn!("seed: the seed store is unreadable: no carried seed");
            return Carried::none(carried_seed::UNREADABLE);
        }
        Stored::Found { record, .. } => record,
    };
    if let Stored::Found { record, .. } = &mut stored {
        aricode_crypto::wipe(&mut record.seed);
    }
    for i in 3..NOISE_SAMPLES {
        stamp(&mut noise, i);
    }
    let (kernel, mut next) = split(&current.seed, &noise);
    aricode_crypto::wipe(&mut current.seed);
    let mut sector = Record {
        generation: p.generation,
        seed: next,
    }
    .encode();
    aricode_crypto::wipe(&mut next);
    let saved = disk.write(first_lba + COPIES[p.write], &sector);
    aricode_crypto::wipe(&mut sector);
    let mut carried = Carried {
        status: carried_seed::CARRIED,
        seed: kernel,
    };
    if !saved {
        warn!(
            "seed: could not save the successor of the carried seed (generation {}): not used, the next boot would find it again",
            current.generation
        );
        aricode_crypto::wipe(&mut carried.seed);
        carried.status = carried_seed::NOT_SAVED;
        return carried;
    }
    // The successor is on the disk with a higher generation: the old copy
    // is blanked so a later damaged successor can never bring it back. A
    // failed blank leaves it, still older than the successor.
    if !disk.write(first_lba + COPIES[p.blank], &[0u8; aricode_ab::SECTOR]) {
        warn!("seed: could not blank the old seed copy (the successor is newer)");
    }
    info!(
        "seed: carried seed of generation {} taken, successor of generation {} saved",
        current.generation, p.generation
    );
    carried
}
