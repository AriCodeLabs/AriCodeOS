// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0

use crate::fake::{self, Bank, Ticks};
use crate::gtt::{self, Cache, Layout};
use crate::power::{self, Forcewake, Form, Limits};
use crate::regs::{self, bcs, masked_off, masked_on};
use crate::{check_blitter, inspect, Refusal, Timeouts};

/// MGGC0 as read on the laptop: 64 MiB stolen, 2 MiB of GTT.
const MGGC0: u32 = 0x0000_0211;
const BAR0: u64 = 4 << 20;

#[test]
fn the_laptop_as_probed_is_accepted() {
    let bank = Bank::probed();
    let found = inspect(&bank, 0x0166, BAR0, MGGC0).unwrap();
    assert_eq!(found.gtt_entries, 512 * 1024);
    assert_eq!(found.rc_state, 0);
    assert_eq!(
        found.limits,
        Limits {
            highest: 21,
            efficient: 7,
            lowest: 7
        }
    );
    assert!(bank.writes.is_empty(), "inspect wrote");
}

#[test]
fn other_states_are_left_alone() {
    let bank = Bank::probed();
    assert_eq!(
        inspect(&bank, 0x0412, BAR0, MGGC0),
        Err(Refusal::NotThisFamily)
    );
    assert_eq!(
        inspect(&bank, 0x0166, 2 << 20, MGGC0),
        Err(Refusal::BadBars)
    );
    assert_eq!(inspect(&bank, 0x0166, BAR0, 0x11), Err(Refusal::GttSize));
    assert_eq!(inspect(&bank, 0x0166, BAR0, 0x311), Err(Refusal::GttSize));
    let mut gone = Bank::probed();
    gone.regs.insert(regs::GT_CORE_STATUS, u32::MAX);
    assert_eq!(inspect(&gone, 0x0166, BAR0, MGGC0), Err(Refusal::NoAnswer));
    let mut caps = Bank::probed();
    caps.regs.insert(regs::RP_STATE_CAP, 0x0015_0707); // lowest above highest
    assert_eq!(
        inspect(&caps, 0x0166, BAR0, MGGC0),
        Err(Refusal::FrequencyLimits)
    );
}

#[test]
fn forcewake_drops_the_firmwares_bits_and_takes_the_multithreaded_form() {
    let mut bank = Bank::probed();
    let clock = Ticks::new();
    let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
    assert_eq!(fw.form(), Form::MultiThreaded);
    // Neither request is held any more: the GT may sleep.
    assert_eq!(bank.get(regs::FORCEWAKE_MT) & 1, 0);
    assert_eq!(bank.get(regs::FORCEWAKE), 0);
    assert_eq!(bank.writes_to(regs::FORCEWAKE_MT)[0], masked_off(0xFFFF));
    // Held while in scope, released after.
    {
        let awake = fw.hold(&mut bank, &clock).unwrap();
        assert_eq!(awake.read(regs::ECOBUS), fake::ECOBUS_AWAKE);
    }
    assert_eq!(bank.get(regs::FORCEWAKE_MT) & 1, 0);
    assert_eq!(
        *bank.writes_to(regs::FORCEWAKE_MT).last().unwrap(),
        masked_off(regs::FORCEWAKE_KERNEL)
    );
}

#[test]
fn without_the_multithreaded_form_the_legacy_one_is_used() {
    let mut bank = Bank::probed();
    bank.ecobus = 0;
    let clock = Ticks::new();
    let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
    assert_eq!(fw.form(), Form::Legacy);
    {
        let _awake = fw.hold(&mut bank, &clock).unwrap();
    }
    assert_eq!(
        bank.writes_to(regs::FORCEWAKE).last(),
        Some(&masked_off(regs::FORCEWAKE_KERNEL))
    );
    assert_eq!(bank.get(regs::FORCEWAKE), 0);
}

#[test]
fn a_forcewake_that_never_answers_is_refused_and_released() {
    let mut bank = Bank::probed();
    let clock = Ticks::new();
    let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
    bank.acks = false;
    assert_eq!(
        fw.hold(&mut bank, &clock).err(),
        Some(Refusal::ForcewakeTimeout)
    );
    assert_eq!(
        bank.writes_to(regs::FORCEWAKE_MT).last(),
        Some(&masked_off(regs::FORCEWAKE_KERNEL)),
        "the request must not stay held"
    );
    // An ack stuck on, as the firmware might leave it, is refused at init.
    let mut stuck = Bank::probed();
    stuck.acks = false;
    assert_eq!(
        Forcewake::init(&mut stuck, &clock, Timeouts::default()).err(),
        Some(Refusal::ForcewakeTimeout)
    );
}

#[test]
fn threads_that_do_not_settle_are_a_timeout() {
    let mut bank = Bank::probed();
    let clock = Ticks::new();
    let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
    bank.regs.insert(regs::GT_THREAD_STATUS, 0x4000_0001);
    assert_eq!(
        fw.hold(&mut bank, &clock).err(),
        Some(Refusal::ForcewakeTimeout)
    );
    assert_eq!(bank.get(regs::FORCEWAKE_MT) & 1, 0);
}

#[test]
fn the_blitter_as_probed_passes_and_any_other_state_does_not() {
    let clock = Ticks::new();
    let cases: [(u32, u32, Option<Refusal>); 6] = [
        (bcs::CTL, 0, None),
        (bcs::CTL, 0x3001, Some(Refusal::RingRunning)),
        (bcs::MODE, bcs::PPGTT_ENABLE, Some(Refusal::PpgttOn)),
        (bcs::MI_MODE, 0, Some(Refusal::NotIdle)),
        (bcs::FAULT, 1, Some(Refusal::Faulted)),
        (
            regs::GDRST,
            regs::GDRST_BLITTER,
            Some(Refusal::ResetPending),
        ),
    ];
    for (offset, value, want) in cases {
        let mut bank = Bank::probed();
        let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
        bank.regs.insert(offset, value);
        let awake = fw.hold(&mut bank, &clock).unwrap();
        assert_eq!(
            check_blitter(&awake).err(),
            want,
            "{offset:#x} = {value:#x}"
        );
    }
}

#[test]
fn entries_encode_as_linux_and_the_firmware_wrote_them() {
    // The laptop's first entry, cache bits aside.
    assert_eq!(gtt::decode(0xDBE0_0001), Some(fake::STOLEN));
    let e = gtt::encode(fake::STOLEN, Cache::Uncached).unwrap();
    assert_eq!(e, 0xDBE0_0003);
    assert_eq!(gtt::decode(e), Some(fake::STOLEN));
    // Bits 39:32 go to 11:4.
    let high = 0xA7_CC25_5000;
    let e = gtt::encode(high, Cache::Llc).unwrap();
    assert_eq!(e, 0xCC25_5A75);
    assert_eq!(gtt::decode(e), Some(high));
    assert_eq!(gtt::encode(1 << 40, Cache::Llc), None);
    assert_eq!(gtt::encode(0x1234, Cache::Llc), None);
    assert_eq!(gtt::decode(0xDBE0_0000), None);
}

#[test]
fn the_firmwares_run_is_found_and_must_cover_the_screen() {
    let bank = Bank::probed();
    let screen = 1376 * 768 * 4;
    assert_eq!(
        gtt::firmware_run(&bank, fake::ENTRIES, fake::STOLEN, screen),
        Ok(fake::FIRMWARE_RUN)
    );
    assert_eq!(
        gtt::firmware_run(&bank, fake::ENTRIES, fake::STOLEN, 64 << 20),
        Err(Refusal::GttSize)
    );
    assert_eq!(
        gtt::firmware_run(&bank, fake::ENTRIES, fake::STOLEN + 4096, screen),
        Err(Refusal::GttSize)
    );
    assert!(bank.writes.is_empty());
}

#[test]
fn clearing_leaves_the_screen_and_replaces_every_stale_entry() {
    let mut bank = Bank::probed();
    let scratch = gtt::encode(0x1000_0000, Cache::Uncached).unwrap();
    let before = bank.gtt[..fake::FIRMWARE_RUN as usize].to_vec();
    gtt::clear(&mut bank, fake::FIRMWARE_RUN, fake::ENTRIES, scratch);
    assert_eq!(&bank.gtt[..fake::FIRMWARE_RUN as usize], &before[..]);
    assert!(bank.gtt[fake::FIRMWARE_RUN as usize..]
        .iter()
        .all(|&e| e == scratch));
    assert_eq!(bank.writes.last(), Some(&(regs::GFX_FLSH_CNTL, 1)));
}

#[test]
fn the_layout_keeps_clear_of_the_firmware_and_aligns_the_directory() {
    let l = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4).unwrap();
    assert_eq!(l.status, fake::FIRMWARE_RUN + 1, "a guard page between");
    assert_eq!(l.ring, l.status + 1);
    assert_eq!(l.end, l.ring + 4);
    assert_eq!(l.directory, fake::ENTRIES - 512);
    assert_eq!(l.directory % 16, 0);
    assert_eq!(l.pp_dir_base(), (l.directory * 4) << 10);
    assert_eq!(Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 0), None);
    assert_eq!(Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 513), None);
    // A ring that would start above 512 MiB.
    assert_eq!(Layout::new(200_000, fake::ENTRIES, 4), None);
    // A table too small for the directory.
    assert_eq!(Layout::new(0, 256, 4), None);
}

#[test]
fn the_frequency_is_asked_with_rps_off_and_within_the_limits() {
    let mut bank = Bank::probed();
    let clock = Ticks::new();
    let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
    let limits = Limits::decode(0x0007_0715).unwrap();
    {
        let mut awake = fw.hold(&mut bank, &clock).unwrap();
        assert_eq!(power::frequency(&awake), 7);
        assert_eq!(power::set_frequency(&mut awake, &limits, 40), 21);
        assert_eq!(power::set_frequency(&mut awake, &limits, 1), 7);
        assert_eq!(power::set_frequency(&mut awake, &limits, 13), 13);
    }
    assert_eq!(bank.get(regs::RP_CONTROL), 0);
    assert_eq!(bank.get(regs::RPNSWREQ), 13 << 25);
    assert_eq!(bank.get(regs::RP_INTERRUPT_LIMITS), 13 << 24 | 7 << 16);
    assert_eq!(13 * power::STEP_MHZ, 650);
    // Every write happened with the GT held awake.
    let mut held = false;
    for &(offset, value) in &bank.writes {
        if offset == regs::FORCEWAKE_MT {
            held = value == masked_on(regs::FORCEWAKE_KERNEL);
        } else if offset < 0x4_0000 && offset != regs::FORCEWAKE {
            assert!(held, "{offset:#x} written while the GT could sleep");
        }
    }
}

mod blitter {
    extern crate std;
    use crate::cmd::{self, BlitError, Rect, Surface};
    use crate::fake::{self, Bank, Ticks};
    use crate::gtt::Layout;
    use crate::power::Forcewake;
    use crate::regs::{self, bcs, masked_on};
    use crate::ring::{self, Ring, REQUEST_DWORDS, SEQNO_INDEX};
    use crate::{Refusal, Timeouts};

    const SCREEN: Surface = Surface {
        address: 0x10_0000,
        pitch: 1376 * 4,
        width: 1366,
        height: 768,
    };

    #[test]
    fn blits_encode_as_linux_builds_them() {
        let r = Rect {
            x1: 10,
            y1: 20,
            x2: 110,
            y2: 70,
        };
        let f = cmd::fill(&SCREEN, &r, 0x00FF_8000).unwrap();
        // Linux: XY_COLOR_BLT_CMD | BLT_WRITE_RGBA | (6 - 2); BLT_DEPTH_32 |
        // BLT_ROP_COLOR_COPY | pitch.
        assert_eq!(f[0], (2 << 29 | 0x50 << 22) | (1 << 21 | 1 << 20) | 4);
        assert_eq!(f[1], 3 << 24 | 0xF0 << 16 | 5504);
        assert_eq!(
            f[2..],
            [20 << 16 | 10, 70 << 16 | 110, 0x10_0000, 0x00FF_8000]
        );
        let src = Surface {
            address: 0x80_0000,
            ..SCREEN
        };
        let c = cmd::copy(&SCREEN, 5, 6, &src, &r).unwrap();
        assert_eq!(c[0], (2 << 29 | 0x53 << 22) | (1 << 21 | 1 << 20) | 6);
        assert_eq!(c[1], 3 << 24 | 0xCC << 16 | 5504);
        assert_eq!(
            c[2..],
            [
                6 << 16 | 5,
                56 << 16 | 105,
                0x10_0000,
                20 << 16 | 10,
                5504,
                0x80_0000
            ]
        );
    }

    #[test]
    fn blits_outside_the_rules_are_refused() {
        let r = Rect {
            x1: 0,
            y1: 0,
            x2: 8,
            y2: 8,
        };
        let bad = |s: Surface| cmd::fill(&s, &r, 0).err();
        assert_eq!(
            bad(Surface {
                pitch: 5502,
                ..SCREEN
            }),
            Some(BlitError::Pitch)
        );
        assert_eq!(
            bad(Surface {
                pitch: 40_000,
                ..SCREEN
            }),
            Some(BlitError::Pitch)
        );
        assert_eq!(
            bad(Surface {
                address: 2,
                ..SCREEN
            }),
            Some(BlitError::Address)
        );
        assert_eq!(
            bad(Surface {
                width: 1377,
                ..SCREEN
            }),
            Some(BlitError::Rect)
        );
        assert_eq!(
            bad(Surface {
                address: 0xFFFF_0000,
                ..SCREEN
            }),
            Some(BlitError::TooLarge)
        );
        let empty = Rect {
            x1: 3,
            y1: 0,
            x2: 3,
            y2: 8,
        };
        assert_eq!(cmd::fill(&SCREEN, &empty, 0).err(), Some(BlitError::Rect));
        let past = Rect {
            x1: 0,
            y1: 760,
            x2: 8,
            y2: 769,
        };
        assert_eq!(cmd::fill(&SCREEN, &past, 0).err(), Some(BlitError::Rect));
        assert_eq!(
            cmd::copy(&SCREEN, 1360, 0, &SCREEN, &r).err(),
            Some(BlitError::Rect)
        );
        // Within one surface: allowed only with a pitch of whole 64 bytes.
        assert!(cmd::copy(&SCREEN, 100, 100, &SCREEN, &r).is_ok());
        let odd = Surface {
            pitch: 1367 * 4,
            ..SCREEN
        };
        assert_eq!(
            cmd::copy(&odd, 100, 100, &odd, &r).err(),
            Some(BlitError::Overlap)
        );
        let near = Surface {
            address: SCREEN.address + 4096,
            ..SCREEN
        };
        assert_eq!(
            cmd::copy(&SCREEN, 0, 0, &near, &r).err(),
            Some(BlitError::Overlap)
        );
    }

    #[test]
    fn a_request_runs_the_batch_non_secure_then_stores_its_number() {
        let mut mem = [0xDEAD_BEEFu32; 1024];
        let mut ring = Ring::new(1);
        let tail = ring.request(&mut mem, 0, 0x40_0000, 7).unwrap();
        assert_eq!(tail as usize, REQUEST_DWORDS * 4);
        assert_eq!(tail % 8, 0, "TAIL moves in quadwords");
        // First the TLB, so a page unmapped since the last batch is gone.
        assert_eq!(mem[0], 0x26 << 23 | 1 | 1 << 21 | 1 << 18 | 1 << 14);
        assert_eq!(mem[1], (0x80 * 4) | (1 << 2));
        assert_eq!(&mem[2..4], &[0, 0]);
        let m = &mem[4..];
        assert_eq!(m[0], 0x31 << 23 | 1 << 8);
        assert_eq!(m[1], 0x40_0000);
        assert_eq!(m[2], 0x26 << 23 | 1 | 1 << 21 | 1 << 18 | 1 << 14);
        assert_eq!(m[3], (SEQNO_INDEX * 4) | (1 << 2));
        assert_eq!(m[4], 7);
        assert_eq!(&m[5..8], &[0x21 << 23 | 1, SEQNO_INDEX * 4, 7]);
        assert_eq!(&mem[REQUEST_DWORDS - 2..REQUEST_DWORDS], &[0x02 << 23, 0]);
        assert_eq!(mem[REQUEST_DWORDS], 0xDEAD_BEEF, "nothing past the request");
    }

    #[test]
    fn the_ring_waits_for_room_and_wraps_with_no_ops() {
        let mut mem = [0u32; 1024];
        let mut ring = Ring::new(1);
        let per = (REQUEST_DWORDS * 4) as u32;
        // With the engine still at 0, the ring takes what fits less the gap.
        let mut n = 0;
        while ring.request(&mut mem, 0, 0x1000, n).is_some() {
            n += 1;
        }
        assert_eq!(n, (4096 - 64) / per);
        // Once the engine has consumed it all, the next wraps to 0.
        let head = ring.tail();
        mem.fill(0xAAAA_AAAA);
        let tail = ring.request(&mut mem, head, 0x2000, 99).unwrap();
        assert_eq!(tail, per);
        assert_eq!(mem[5], 0x2000);
        assert!(mem[(head / 4) as usize..].iter().all(|&w| w == cmd::NOOP));
        assert!(ring::passed(5, 5) && ring::passed(6, 5) && !ring::passed(4, 5));
        assert!(ring::passed(2, u32::MAX), "numbers wrap");
    }

    fn started() -> (Bank, Forcewake, Layout) {
        let mut bank = Bank::probed();
        let clock = Ticks::new();
        let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
        let layout = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4).unwrap();
        (bank, fw, layout)
    }

    #[test]
    fn the_blitter_starts_on_an_empty_ring_in_the_ppgtt() {
        let (mut bank, fw, layout) = started();
        let clock = Ticks::new();
        let mut ring = Ring::new(4);
        {
            let mut awake = fw.hold(&mut bank, &clock).unwrap();
            ring::prepare(&mut awake);
            ring::start(&mut awake, &clock, &Timeouts::default(), &layout, &mut ring).unwrap();
        }
        assert_eq!(bank.get(bcs::HWS_PGA), layout.status * 4096);
        assert_eq!(bank.get(bcs::START), layout.ring * 4096);
        assert_eq!(bank.get(bcs::CTL), 3 << 12 | 1);
        assert_eq!(bank.get(bcs::PP_DIR_BASE), layout.pp_dir_base());
        assert_eq!(bank.get(bcs::PP_DIR_DCLV), 0xFFFF_FFFF);
        assert_eq!(bank.get(bcs::MODE) & bcs::PPGTT_ENABLE, bcs::PPGTT_ENABLE);
        assert_eq!(bank.get(bcs::MI_MODE) & bcs::MODE_STOP, 0);
        assert_eq!(bank.get(bcs::HWSTAM), !0);
        assert_eq!(bank.get(regs::RCS_GFX_MODE) & 0x2800, 0x2800);
        assert_eq!(bank.writes_to(bcs::INSTPM), [masked_on(1 << 9 | 1 << 5)]);
        // The ring is turned on last but one, after its start and head.
        let order: std::vec::Vec<u32> = bank.writes.iter().map(|w| w.0).collect();
        let pos = |r| order.iter().rposition(|&o| o == r).unwrap();
        assert!(pos(bcs::START) < pos(bcs::CTL) && pos(bcs::PP_DIR_BASE) < pos(bcs::CTL));
        assert!(pos(bcs::CTL) < pos(bcs::MI_MODE));
    }

    #[test]
    fn a_ring_that_will_not_take_its_head_is_not_started() {
        let (mut bank, fw, layout) = started();
        let clock = Ticks::new();
        bank.regs.insert(bcs::HEAD, 0x40);
        bank.stuck_head = true;
        let mut ring = Ring::new(4);
        let mut awake = fw.hold(&mut bank, &clock).unwrap();
        let r = ring::start(&mut awake, &clock, &Timeouts::default(), &layout, &mut ring);
        assert_eq!(r, Err(Refusal::RingRunning));
        drop(awake);
        assert_eq!(bank.get(bcs::CTL) & 1, 0, "never turned on");
    }

    #[test]
    fn a_reset_stops_the_engine_and_resets_its_domain_twice() {
        let (mut bank, fw, _) = started();
        let clock = Ticks::new();
        {
            let mut awake = fw.hold(&mut bank, &clock).unwrap();
            ring::reset(&mut awake, &clock, &Timeouts::default()).unwrap();
        }
        assert_eq!(bank.writes_to(regs::GDRST), [regs::GDRST_BLITTER; 2]);
        assert_eq!(bank.get(bcs::CTL), 0);
        let (mut bank, fw, _) = started();
        bank.completes = false;
        let mut awake = fw.hold(&mut bank, &clock).unwrap();
        assert_eq!(
            ring::reset(&mut awake, &clock, &Timeouts::default()),
            Err(Refusal::Timeout)
        );
    }
}

#[test]
fn the_directory_lives_in_the_tables_own_slots() {
    use crate::ppgtt;
    let mut bank = Bank::probed();
    let layout = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4).unwrap();
    let pde = ppgtt::directory_entry(0x1_2345_6000).unwrap();
    assert_eq!(pde, 0x2345_6011, "no cache bits");
    assert_eq!(ppgtt::directory_entry(0x1234), None);
    assert_eq!(ppgtt::set_directory(&mut bank, &layout, 511, pde), Some(()));
    assert_eq!(bank.gtt[(layout.directory + 511) as usize], pde);
    assert_eq!(ppgtt::set_directory(&mut bank, &layout, 512, pde), None);
    assert_eq!(ppgtt::split(0x0040_3000), (1, 3));
    assert_eq!(ppgtt::split(0x7FFF_F000), (511, 1023));
    assert_eq!(ppgtt::SIZE, 2 << 30);
}

#[test]
fn every_refusal_has_a_name_in_diag() {
    use aricode_abi::sysinfo::gpu_refusal;
    let all = [
        Refusal::NotThisFamily,
        Refusal::BadBars,
        Refusal::NoAnswer,
        Refusal::RingRunning,
        Refusal::PpgttOn,
        Refusal::ResetPending,
        Refusal::ForcewakeTimeout,
        Refusal::NotIdle,
        Refusal::Faulted,
        Refusal::GttSize,
        Refusal::FrequencyLimits,
        Refusal::Timeout,
        Refusal::NoMemory,
    ];
    for (i, r) in all.iter().enumerate() {
        assert_eq!(*r as u8, i as u8 + 1, "numbered in order");
        assert!(gpu_refusal(*r as u8).is_some(), "{r:?}");
    }
    assert_eq!(gpu_refusal(all.len() as u8 + 1), None);
}
