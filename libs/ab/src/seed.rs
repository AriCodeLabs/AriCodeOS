// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel seed carried across boots (ADR 0043): two copies of a 32-byte
//! seed in sectors 1 and 2 of the `ARIBOOT` partition (sector 0 is the
//! boot state). Shared by AriBoot, which reads the seed, writes its
//! successor and only then hands the kernel its share, by init, which
//! replaces it twice per boot with output of the kernel generator, and by
//! xtask (images ship none; the two-boot scenario checks it changed).
//!
//! Every write goes to the copy not in use, with the next generation, and
//! only then is the old copy blanked: a crash at any point leaves at least
//! one copy that decodes, and never one the kernel already used (AriBoot
//! writes the successor before the jump; a successor that did not reach
//! the disk means the kernel never got the old seed).

use crate::SECTOR;
use aricode_crypto::hmac::hmac_sha256;
use aricode_crypto::sha256::sha256;

pub const SEED_MAGIC: &[u8; 8] = b"ARISEED\0";
/// Format version (byte 8).
const VERSION: u8 = 1;
/// Sectors of the `ARIBOOT` partition holding the two copies.
pub const COPIES: [u64; 2] = [1, 2];
pub const SEED_LEN: usize = 32;
/// Bytes covered by the check (`CHECK_AT..CHECK_AT + 8` holds it).
const CHECK_AT: usize = 56;

/// One copy: the generation (grows by one per write) and the seed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub generation: u64,
    pub seed: [u8; SEED_LEN],
}

/// No `Debug` with the seed in it: a `{:?}` must never print it.
impl core::fmt::Debug for Record {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Record")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Record {
    /// Sector layout: magic, version, 7 zero bytes, generation (LE),
    /// seed, the first 8 bytes of SHA-256 over bytes 0..56, zeros.
    pub fn encode(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(SEED_MAGIC);
        s[8] = VERSION;
        s[16..24].copy_from_slice(&self.generation.to_le_bytes());
        s[24..CHECK_AT].copy_from_slice(&self.seed);
        let check = sha256(&s[..CHECK_AT]);
        s[CHECK_AT..CHECK_AT + 8].copy_from_slice(&check[..8]);
        s
    }

    /// `None` unless the sector is a whole, intact record.
    pub fn decode(s: &[u8]) -> Option<Self> {
        if s.len() < SECTOR || &s[..8] != SEED_MAGIC || s[8] != VERSION {
            return None;
        }
        if s[9..16].iter().any(|&b| b != 0) || s[CHECK_AT + 8..SECTOR].iter().any(|&b| b != 0) {
            return None;
        }
        let check = sha256(&s[..CHECK_AT]);
        if s[CHECK_AT..CHECK_AT + 8] != check[..8] {
            return None;
        }
        let mut seed = [0u8; SEED_LEN];
        seed.copy_from_slice(&s[24..CHECK_AT]);
        Some(Self {
            generation: u64::from_le_bytes(s[16..24].try_into().ok()?),
            seed,
        })
    }
}

/// What the two copies hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    /// Both copies blank (all zero): no seed was ever written, the first
    /// boot of an image (which ships none).
    Missing,
    /// No copy decodes and at least one is not blank or could not be read.
    Unreadable,
    /// The copy to use: the one that decodes with the highest generation
    /// (copy 0 on a tie).
    Found { copy: usize, record: Record },
}

/// Picks the seed from the two copies as read (`None`: the read failed).
pub fn pick(copies: [Option<&[u8]>; 2]) -> Stored {
    let mut best: Option<(usize, Record)> = None;
    let mut blank = 0;
    for (i, c) in copies.iter().enumerate() {
        let Some(s) = c else { continue };
        if let Some(r) = Record::decode(s) {
            if best.is_none_or(|(_, b)| r.generation > b.generation) {
                best = Some((i, r));
            }
        } else if s.iter().all(|&b| b == 0) {
            blank += 1;
        }
    }
    match best {
        Some((copy, record)) => Stored::Found { copy, record },
        None if blank == copies.len() => Stored::Missing,
        None => Stored::Unreadable,
    }
}

/// Where a successor goes: `write` gets it with `generation`, then
/// `blank` is zeroed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub write: usize,
    pub generation: u64,
    pub blank: usize,
}

/// The successor of what `pick` found goes to the other copy.
pub fn plan(current: &Stored) -> Plan {
    match current {
        Stored::Found { copy, record } => Plan {
            write: 1 - copy,
            generation: record.generation.saturating_add(1),
            blank: *copy,
        },
        Stored::Missing | Stored::Unreadable => Plan {
            write: 0,
            generation: 1,
            blank: 1,
        },
    }
}

/// The kernel's share of a carried seed and its successor for the next
/// boot, both HMAC-SHA-256 keyed with the seed over a label and the
/// loader's noise (cycle counter samples): one-way, so the successor on
/// disk reveals neither the seed nor the kernel's share.
pub fn split(seed: &[u8; SEED_LEN], noise: &[u8]) -> ([u8; 32], [u8; 32]) {
    (
        hmac_sha256(seed, &[b"AriCodeOS carried seed: kernel", noise]),
        hmac_sha256(seed, &[b"AriCodeOS carried seed: next boot", noise]),
    )
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::format;

    fn rec(generation: u64, fill: u8) -> Record {
        Record {
            generation,
            seed: [fill; SEED_LEN],
        }
    }

    #[test]
    fn record_round_trips_and_rejects_damage() {
        let r = rec(7, 0xA5);
        let s = r.encode();
        assert_eq!(&s[..8], SEED_MAGIC);
        assert_eq!(Record::decode(&s), Some(r));
        // Every single-byte change in the covered part is caught, and so is
        // anything in the bytes that must be zero.
        for at in [0, 8, 9, 15, 16, 23, 24, 55, 56, 63, 64, 511] {
            let mut bad = s;
            bad[at] ^= 1;
            assert_eq!(Record::decode(&bad), None, "byte {at}");
        }
        assert_eq!(Record::decode(&s[..511]), None, "short");
        assert_eq!(Record::decode(&[0u8; SECTOR]), None, "blank");
        // The boot state's sector is not a seed.
        let state = crate::BootState::fresh(crate::Slot::A).encode();
        assert_eq!(Record::decode(&state), None);
    }

    #[test]
    fn debug_never_shows_the_seed() {
        let text = format!("{:?}", rec(3, 0xAB));
        assert!(text.contains("generation: 3"), "{text}");
        assert!(
            !text.contains("171") && !text.to_lowercase().contains("ab,"),
            "{text}"
        );
    }

    #[test]
    fn pick_takes_the_newest_valid_copy() {
        let blank = [0u8; SECTOR];
        let garbage = [0x5Au8; SECTOR];
        let (a, b) = (rec(4, 1).encode(), rec(5, 2).encode());
        assert_eq!(pick([Some(&blank), Some(&blank)]), Stored::Missing);
        assert_eq!(pick([Some(&garbage), Some(&blank)]), Stored::Unreadable);
        assert_eq!(pick([None, Some(&blank)]), Stored::Unreadable, "read error");
        assert_eq!(pick([None, None]), Stored::Unreadable);
        assert_eq!(
            pick([Some(&a), Some(&blank)]),
            Stored::Found {
                copy: 0,
                record: rec(4, 1)
            }
        );
        // A crash between writing the successor and blanking the old copy
        // leaves both: the newer one wins.
        assert_eq!(
            pick([Some(&a), Some(&b)]),
            Stored::Found {
                copy: 1,
                record: rec(5, 2)
            }
        );
        // A torn successor next to the old copy: the old copy, which the
        // kernel never got (the loader jumps only after the write).
        assert_eq!(
            pick([Some(&garbage), Some(&b)]),
            Stored::Found {
                copy: 1,
                record: rec(5, 2)
            }
        );
        assert_eq!(
            pick([Some(&b), None]),
            Stored::Found {
                copy: 0,
                record: rec(5, 2)
            }
        );
    }

    #[test]
    fn plan_writes_the_other_copy_then_blanks_the_old() {
        let found = Stored::Found {
            copy: 1,
            record: rec(9, 0),
        };
        assert_eq!(
            plan(&found),
            Plan {
                write: 0,
                generation: 10,
                blank: 1
            }
        );
        for s in [Stored::Missing, Stored::Unreadable] {
            assert_eq!(
                plan(&s),
                Plan {
                    write: 0,
                    generation: 1,
                    blank: 1
                }
            );
        }
        let last = Stored::Found {
            copy: 0,
            record: rec(u64::MAX, 0),
        };
        assert_eq!(plan(&last).generation, u64::MAX);
    }

    /// The whole cycle on a model disk, with a crash at every step: the
    /// seed the kernel was handed is never the one the next boot finds.
    #[test]
    fn a_crash_never_leaves_a_used_seed() {
        let mut disk = [[0u8; SECTOR]; 2];
        disk[0] = rec(1, 0x11).encode();
        // Boot: read, split, write successor, blank old, jump.
        for crash_after in 0..3 {
            let mut d = disk;
            let current = pick([Some(&d[0]), Some(&d[1])]);
            let Stored::Found { record, .. } = current else {
                panic!("no seed")
            };
            let (kernel, next) = split(&record.seed, b"noise");
            let p = plan(&current);
            let mut handed = None;
            if crash_after >= 1 {
                d[p.write] = Record {
                    generation: p.generation,
                    seed: next,
                }
                .encode();
            }
            if crash_after >= 2 {
                d[p.blank] = [0; SECTOR];
                handed = Some(kernel);
            }
            let after = pick([Some(&d[0]), Some(&d[1])]);
            let Stored::Found { record: r2, .. } = after else {
                panic!("crash {crash_after}: seed lost")
            };
            if let Some(k) = handed {
                assert_ne!(r2.seed, record.seed, "crash {crash_after}");
                assert_ne!(split(&r2.seed, b"noise").0, k, "crash {crash_after}");
            }
            if crash_after >= 1 {
                assert_eq!(r2.generation, 2, "crash {crash_after}: successor found");
            }
        }
    }

    #[test]
    fn split_is_one_way_and_depends_on_everything() {
        let seed = [7u8; 32];
        let (k, n) = split(&seed, b"tsc");
        assert_ne!(k, n);
        assert_ne!(k, seed);
        assert_ne!(n, seed);
        assert_ne!(split(&seed, b"tsd"), (k, n), "noise");
        let mut other = seed;
        other[31] ^= 1;
        assert_ne!(split(&other, b"tsc").0, k, "seed");
    }
}
