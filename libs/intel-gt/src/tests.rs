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
    // A screen that does not start at 0 must be covered to its end (audit M1).
    let at = (fake::FIRMWARE_RUN as u64 - 100) * 4096;
    assert_eq!(
        gtt::firmware_run(&bank, fake::ENTRIES, fake::STOLEN, at + screen),
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
    // The laptop: 64 MiB kept, a 256 MiB aperture.
    const APERTURE: u32 = (256 << 20) / 4096;
    let l = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4, APERTURE).unwrap();
    assert_eq!(
        l.status,
        APERTURE + 1,
        "past the aperture, a guard page between"
    );
    let small = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4, 4096).unwrap();
    assert_eq!(
        small.status,
        fake::FIRMWARE_RUN + 1,
        "past the firmware's run"
    );
    assert_eq!(l.ring, l.status + 1);
    assert_eq!(
        l.render_status,
        l.ring + 4,
        "the render engine's pages after"
    );
    assert_eq!(l.render_ring, l.render_status + 1);
    assert_eq!(l.end, l.render_ring + 4);
    assert_eq!(l.placement(&regs::RENDER), (l.render_status, l.render_ring));
    assert_eq!(l.placement(&regs::BLITTER), (l.status, l.ring));
    assert_eq!(l.directory, fake::ENTRIES - 512);
    assert_eq!(l.directory % 16, 0);
    assert_eq!(l.pp_dir_base(), (l.directory * 4) << 10);
    assert_eq!(
        Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 0, APERTURE),
        None
    );
    assert_eq!(
        Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 513, APERTURE),
        None
    );
    // A ring that would start above 512 MiB: a 512 MiB aperture, or a run
    // that long.
    assert_eq!(
        Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4, 131_072),
        None
    );
    assert_eq!(Layout::new(200_000, fake::ENTRIES, 4, APERTURE), None);
    // A table too small for the directory.
    assert_eq!(Layout::new(0, 256, 4, 0), None);
}

#[test]
fn the_frequency_is_asked_with_rps_on_and_within_the_limits() {
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
    assert_eq!(bank.get(regs::RP_CONTROL), 0xD92, "on, as Linux sets it");
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
    use crate::ring::{self, Ring, REQUEST_EXTRA, SEQNO_INDEX};
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
    fn a_request_writes_the_commands_then_stores_its_number() {
        let mut mem = [0xDEAD_BEEFu32; 1024];
        let mut ring = Ring::new(1);
        let body = [cmd::NOOP, cmd::FLUSH_DW, 0, 0];
        let tail = ring.request(&mut mem, 0, &body, 7).unwrap();
        // The odd-length no-op is not needed.
        let len = body.len() + REQUEST_EXTRA - 1;
        assert_eq!(tail as usize, len * 4);
        assert_eq!(tail % 8, 0, "TAIL moves in quadwords");
        // First the TLB, so a page unmapped since the last request is gone.
        assert_eq!(mem[0], 0x26 << 23 | 1 | 1 << 21 | 1 << 18 | 1 << 14);
        assert_eq!(mem[1], (0x80 * 4) | (1 << 2));
        assert_eq!(&mem[2..4], &[0, 0]);
        // The commands themselves, never a jump to memory a client reaches.
        assert_eq!(&mem[4..8], &body);
        assert!(!mem[..len].contains(&(cmd::BATCH_START | cmd::BATCH_PPGTT)));
        let m = &mem[8..];
        assert_eq!(m[0], 0x26 << 23 | 1 | 1 << 21 | 1 << 18 | 1 << 14);
        assert_eq!(m[1], (SEQNO_INDEX * 4) | (1 << 2));
        assert_eq!(m[2], 7);
        assert_eq!(&m[3..6], &[0x21 << 23 | 1, SEQNO_INDEX * 4, 7]);
        assert_eq!(&mem[len - 2..len], &[0x02 << 23, 0]);
        assert_eq!(mem[len], 0xDEAD_BEEF, "nothing past the request");
        // An odd number of commands is padded to a quadword before the end.
        let mut ring = Ring::new(1);
        mem.fill(0xDEAD_BEEF);
        let tail = ring.request(&mut mem, 0, &[cmd::NOOP; 3], 8).unwrap();
        assert_eq!(tail as usize, (3 + REQUEST_EXTRA) * 4);
        assert_eq!(
            &mem[7..9],
            &[cmd::NOOP, 0x26 << 23 | 1 | 1 << 21 | 1 << 18 | 1 << 14]
        );
    }

    #[test]
    fn the_ring_waits_for_room_and_wraps_with_no_ops() {
        let mut mem = [0u32; 1024];
        let mut ring = Ring::new(1);
        let body = [cmd::FLUSH_DW, 0, 0, cmd::NOOP];
        let per = ((body.len() + REQUEST_EXTRA - 1) * 4) as u32;
        // With the engine still at 0, the ring takes what fits less the gap.
        let mut n = 0;
        while ring.request(&mut mem, 0, &body, n).is_some() {
            n += 1;
        }
        assert_eq!(n, (4096 - 64) / per);
        // Once the engine has consumed it all, the next wraps to 0.
        let head = ring.tail();
        mem.fill(0xAAAA_AAAA);
        let tail = ring.request(&mut mem, head, &body, 99).unwrap();
        assert_eq!(tail, per);
        assert_eq!(&mem[4..8], &body);
        assert!(mem[(head / 4) as usize..].iter().all(|&w| w == cmd::NOOP));
        // A request as big as the ring never fits.
        let mut ring = Ring::new(1);
        assert_eq!(ring.request(&mut mem, 0, &[cmd::NOOP; 1024], 1), None);
        assert!(ring::passed(5, 5) && ring::passed(6, 5) && !ring::passed(4, 5));
        assert!(ring::passed(2, u32::MAX), "numbers wrap");
    }

    fn started() -> (Bank, Forcewake, Layout) {
        let mut bank = Bank::probed();
        let clock = Ticks::new();
        let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
        let layout = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4, (256 << 20) / 4096).unwrap();
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
            ring::start(
                &mut awake,
                &clock,
                &Timeouts::default(),
                &regs::BLITTER,
                &layout,
                &mut ring,
            )
            .unwrap();
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
        let r = ring::start(
            &mut awake,
            &clock,
            &Timeouts::default(),
            &regs::BLITTER,
            &layout,
            &mut ring,
        );
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
            ring::reset(&mut awake, &clock, &Timeouts::default(), &regs::BLITTER).unwrap();
        }
        assert_eq!(bank.writes_to(regs::GDRST), [regs::GDRST_BLITTER; 2]);
        assert_eq!(bank.get(bcs::CTL), 0);
        let (mut bank, fw, _) = started();
        bank.completes = false;
        let mut awake = fw.hold(&mut bank, &clock).unwrap();
        assert_eq!(
            ring::reset(&mut awake, &clock, &Timeouts::default(), &regs::BLITTER),
            Err(Refusal::Timeout)
        );
    }
}

#[test]
fn the_directory_lives_in_the_tables_own_slots() {
    use crate::ppgtt;
    let mut bank = Bank::probed();
    let layout = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4, (256 << 20) / 4096).unwrap();
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
        Refusal::Partial,
    ];
    for (i, r) in all.iter().enumerate() {
        assert_eq!(*r as u8, i as u8 + 1, "numbered in order");
        assert!(gpu_refusal(*r as u8).is_some(), "{r:?}");
    }
    assert_eq!(gpu_refusal(all.len() as u8 + 1), None);
}

mod batches {
    extern crate std;
    use crate::check::{check, Fault, Range};
    use crate::cmd::{self, Rect, Surface};

    const START: u32 = 0x8000;
    const MAPS: [Range; 2] = [
        Range {
            start: 0x10_0000,
            end: 0x40_0000,
        },
        Range {
            start: 0x50_0000,
            end: 0x80_0000,
        },
    ];
    const DST: Surface = Surface {
        address: 0x50_0000,
        pitch: 4096,
        width: 1024,
        height: 768,
    };
    const SRC: Surface = Surface {
        address: 0x10_0000,
        pitch: 4096,
        width: 1024,
        height: 768,
    };
    const ALL: Rect = Rect {
        x1: 0,
        y1: 0,
        x2: 1024,
        y2: 768,
    };

    fn batch(parts: &[&[u32]]) -> std::vec::Vec<u32> {
        parts.iter().flat_map(|p| p.iter().copied()).collect()
    }

    #[test]
    fn what_the_client_library_writes_passes() {
        let fill = cmd::fill(&DST, &ALL, 0x0033_6699).unwrap();
        let copy = cmd::copy(&DST, 0, 0, &SRC, &ALL).unwrap();
        let flush = [cmd::FLUSH_DW, 0, 0];
        let b = batch(&[
            &fill,
            &[cmd::NOOP],
            &copy,
            &flush,
            &[cmd::BATCH_END, 0xFFFF_FFFF],
        ]);
        let len = b.len();
        assert_eq!(
            check(&b, &MAPS),
            Ok(len - 2),
            "the commands before the end, which is not written"
        );
    }

    #[test]
    fn what_reached_the_registers_on_the_laptop_is_refused() {
        // ADR 0083's measurement, each part on its own.
        let lri = [(0x22 << 23) | 1, 0x2_217C, 0x00AB_0000, cmd::BATCH_END];
        assert_eq!(check(&lri, &MAPS), Err(Fault::Command(0)));
        let store_index = [
            cmd::FLUSH_DW | cmd::FLUSH_DW_STORE_INDEX | cmd::FLUSH_DW_STORE_DWORD,
            0x240 | cmd::FLUSH_DW_GLOBAL_GTT,
            0xBAD0_0002,
            cmd::BATCH_END,
        ];
        assert_eq!(check(&store_index, &MAPS), Err(Fault::Command(0)));
        let sdi = [(0x20 << 23) | (1 << 22) | 2, 0, 0x240, 1, cmd::BATCH_END];
        assert_eq!(check(&sdi, &MAPS), Err(Fault::Command(0)));
        // A flush that names an address or data, even without the store bits.
        assert_eq!(
            check(&[cmd::FLUSH_DW, 4, 0, cmd::BATCH_END], &MAPS),
            Err(Fault::Command(0))
        );
        // Any jump (the ring runs commands, not batches; ADR 0086), a
        // secure one, MI_UPDATE_GTT.
        for to in [0, 0x8000, 0x10_0000] {
            let jump = [cmd::BATCH_START | cmd::BATCH_PPGTT, to, cmd::BATCH_END];
            assert_eq!(check(&jump, &MAPS), Err(Fault::Command(0)));
        }
        assert_eq!(
            check(&[cmd::BATCH_START, START], &MAPS),
            Err(Fault::Command(0))
        );
        assert_eq!(
            check(&[(0x23 << 23) | 1, 0, 0], &MAPS),
            Err(Fault::Command(0))
        );
    }

    #[test]
    fn blits_must_stay_inside_the_mappings_to_the_byte() {
        let fill = |s: &Surface, r: &Rect| {
            let f = cmd::fill(s, r, 1).unwrap();
            check(&batch(&[&f, &[cmd::BATCH_END]]), &MAPS)
        };
        // The last byte of a mapping is fine; one more is not.
        let edge = Surface {
            address: 0x80_0000 - 4096 * 768,
            ..DST
        };
        assert!(fill(&edge, &ALL).is_ok());
        let past = Surface {
            address: 0x80_0000 - 4096 * 768 + 4,
            ..DST
        };
        assert_eq!(fill(&past, &ALL), Err(Fault::Outside(0)));
        // Across two mappings with a gap between: outside.
        let across = Surface {
            address: 0x3F_F000,
            ..DST
        };
        assert_eq!(
            fill(
                &across,
                &Rect {
                    x1: 0,
                    y1: 0,
                    x2: 1024,
                    y2: 2
                }
            ),
            Err(Fault::Outside(0))
        );
        // The kernel's copy, below every mapping.
        let onto = Surface {
            address: START,
            ..DST
        };
        assert_eq!(
            fill(
                &onto,
                &Rect {
                    x1: 0,
                    y1: 0,
                    x2: 4,
                    y2: 1
                }
            ),
            Err(Fault::Outside(0))
        );
        // A copy whose source is outside.
        let copy = cmd::copy(
            &DST,
            0,
            0,
            &Surface {
                address: 0x8_0000,
                ..SRC
            },
            &ALL,
        )
        .unwrap();
        assert_eq!(
            check(&batch(&[&copy, &[cmd::BATCH_END]]), &MAPS),
            Err(Fault::Outside(0))
        );
    }

    #[test]
    fn fields_the_list_does_not_allow_are_refused() {
        let f = cmd::fill(&DST, &ALL, 1).unwrap();
        let with = |k: usize, v: u32| {
            let mut b = batch(&[&f, &[cmd::BATCH_END]]);
            b[k] = v;
            check(&b, &MAPS)
        };
        assert_eq!(with(0, f[0] | 1 << 11), Err(Fault::Command(0)), "tiled");
        assert_eq!(with(1, f[1] | 1 << 30), Err(Fault::Command(0)), "clipping");
        assert_eq!(
            with(1, f[1] & !(3 << 24)),
            Err(Fault::Command(0)),
            "8 bits a pixel"
        );
        assert_eq!(
            with(1, (f[1] & !0xFFFF) | 0x8000),
            Err(Fault::Command(0)),
            "negative pitch"
        );
        assert_eq!(with(2, 0x8000), Err(Fault::Command(0)), "negative x");
        assert_eq!(with(3, f[2]), Err(Fault::Command(0)), "empty");
        let copy = cmd::copy(&DST, 0, 0, &SRC, &ALL).unwrap();
        let mut b = batch(&[&copy, &[cmd::BATCH_END]]);
        b[6] |= 1 << 16;
        assert_eq!(
            check(&b, &MAPS),
            Err(Fault::Command(0)),
            "source pitch high bits"
        );
        // Audit L1: only the PRM's raster operation for each, aligned
        // addresses, and overlap only as the PRM defines it.
        assert_eq!(
            with(1, (f[1] & !(0xFF << 16)) | 0xCC << 16),
            Err(Fault::Command(0)),
            "rop"
        );
        assert_eq!(with(4, f[4] + 2), Err(Fault::Command(0)), "unaligned");
        let mut b = batch(&[&copy, &[cmd::BATCH_END]]);
        b[1] = (b[1] & !(0xFF << 16)) | 0xF0 << 16;
        assert_eq!(check(&b, &MAPS), Err(Fault::Command(0)), "copy rop");
        let rows = Rect {
            x1: 0,
            y1: 0,
            x2: 1024,
            y2: 8,
        };
        let b = batch(&[
            &cmd::copy(&DST, 0, 4, &DST, &rows).unwrap(),
            &[cmd::BATCH_END],
        ]);
        assert!(check(&b, &MAPS).is_ok(), "one surface, pitch of 64s");
        let mut raw = cmd::copy(&DST, 0, 0, &DST, &rows).unwrap();
        raw[7] = DST.address + 4096; // another base over the same bytes
        let b = batch(&[&raw, &[cmd::BATCH_END]]);
        assert_eq!(
            check(&b, &MAPS),
            Err(Fault::Command(0)),
            "overlap, two bases"
        );
    }

    #[test]
    fn a_batch_must_end_within_itself() {
        let f = cmd::fill(&DST, &ALL, 1).unwrap();
        assert_eq!(check(&f[..5], &MAPS), Err(Fault::Unended(0)));
        assert_eq!(check(&f, &MAPS), Err(Fault::Unended(6)));
        assert_eq!(check(&[], &MAPS), Err(Fault::Unended(0)));
        assert_eq!(check(&[cmd::FLUSH_DW, 0], &MAPS), Err(Fault::Unended(0)));
    }
}

#[test]
fn the_heat_brake_holds_until_the_margin_below() {
    use crate::power::too_hot;
    assert!(!too_hot(false, 84, 85, 5));
    assert!(too_hot(false, 85, 85, 5), "on at the threshold");
    assert!(too_hot(true, 81, 85, 5), "held within the margin");
    assert!(too_hot(true, 80, 85, 5));
    assert!(!too_hot(true, 79, 85, 5), "let go below it");
}

#[test]
fn rps_interrupts_are_quieted_whatever_the_firmware_left() {
    let mut bank = Bank::probed();
    bank.regs.insert(regs::PMINTRMSK, regs::PM_UP_EI_EXPIRED);
    bank.regs.insert(regs::PMIER, 0x0300_0076);
    bank.regs.insert(regs::PMIMR, 0);
    let clock = Ticks::new();
    let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
    {
        let mut awake = fw.hold(&mut bank, &clock).unwrap();
        power::quiet_rps_interrupts(&mut awake);
    }
    assert_eq!(
        bank.get(regs::PMINTRMSK),
        0xFFFF_FFFB,
        "bit 2 clear, the rest masked, as Linux"
    );
    assert_eq!(bank.get(regs::PMIER), 0);
    assert_eq!(bank.get(regs::PMIMR), 0xFFFF_FFFF);
}

mod render_engine {
    use super::*;
    use crate::render::{self, Change, Setting};

    /// What the laptop's firmware left (dev build 55's probe).
    fn probed_render(bank: &mut Bank) {
        for (r, v) in [
            (0x7010, 0),
            (0xB01C, 0x8C47_FF80),
            (0xB030, 0x6000_0000),
            (0xB034, 0x0800_0000),
            (0x2090, 0),
            (0x20A0, 0x28A0_1010),
            (0x7008, 0),
            (0x209C, 0x200),
            (0xE4F4, 0),
            (0xF4F4, 0),
            (0x9404, 0),
            (0x9030, 0x400),
            (0x900C, 0x0040_0000),
            (0x20C0, 0x8000),
            (0x7000, 0x6804),
            (0x7004, 0x0180),
        ] {
            bank.regs.insert(r, v);
        }
    }

    #[test]
    fn linuxs_settings_take_from_the_firmwares_state() {
        let mut bank = Bank::probed();
        probed_render(&mut bank);
        let clock = Ticks::new();
        let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
        {
            let mut awake = fw.hold(&mut bank, &clock).unwrap();
            render::apply(&mut awake, &render::GT);
            render::apply(&mut awake, &render::RENDER);
            render::apply(&mut awake, &render::CLOCK_GATING);
            render::apply(&mut awake, &render::CONTEXT);
        }
        let want = [
            (0xB01C, 0x3C47_FF8C),
            (0xB030, 0x2000_0000),
            (0xB034, 0),
            (0x7010, 0),
            (0x2090, 1 << 10),
            (0x20A0, 0x28A0_1010 & !0x0007_7070),
            (0x7008, 1 << 9),
            (0x209C, 0x200 | 1 << 14),
            (0xE4F4, 1),
            (0xF4F4, 1),
            (0x9404, 1 << 13),
            (0x9030, 0x400 | 1 << 11),
            (0x900C, 1 << 21),
            (0x20C0, 0x8000 | 1 << 7),
            (0x7000, 0x6804),
            (0x7004, 0x0180 | 1 << 6),
        ];
        for (r, v) in want {
            assert_eq!(bank.get(r), v, "{r:#x}");
        }
        assert_eq!(bank.get(0x229C) & 0x2800, 0x2800);
        // No display register among them.
        for st in render::GT
            .iter()
            .chain(&render::RENDER)
            .chain(&render::CLOCK_GATING)
        {
            assert!(st.register < 0x4_0000, "{:#x}", st.register);
        }
    }

    #[test]
    fn the_context_settings_load_from_the_ring() {
        let mut out = [0u32; 7];
        assert_eq!(render::load_masked(&render::CONTEXT, &mut out), Some(7));
        assert_eq!(out[0], 0x22 << 23 | 5, "Linux MI_LOAD_REGISTER_IMM(3)");
        assert_eq!(
            &out[1..],
            &[
                0x20C0,
                1 << 23 | 1 << 7,
                0x7000,
                1 << 16,
                0x7004,
                1 << 22 | 1 << 6
            ]
        );
        let read_first = [Setting {
            register: 0x9030,
            change: Change::Set(1),
        }];
        assert_eq!(render::load_masked(&read_first, &mut out), None);
        assert_eq!(render::load_masked(&render::CONTEXT, &mut out[..6]), None);
    }

    #[test]
    fn pipe_controls_encode_as_linux_emits_them() {
        // GFX_OP_PIPE_CONTROL(4).
        assert_eq!(render::PIPE_CONTROL, 0x7A00_0002);
        assert_eq!(render::stall(), [0x7A00_0002, 1 << 20 | 1 << 1, 0, 0]);
        let b = render::breadcrumb(0x1000_0100, 9);
        assert_eq!(
            b[1],
            1 << 12 | 1 << 0 | 1 << 5 | 1 << 7 | 1 << 14 | 1 << 24 | 1 << 20
        );
        assert_eq!(&b[2..], &[0x1000_0100, 9, 0x02 << 23, 0]);
        let f = render::flush_invalidate(0x2000);
        assert_eq!(
            f[1] & (1 << 23 | 1 << 21 | 1 << 8),
            0,
            "no register write, index store or notify"
        );
        assert_eq!(
            f[1] & (1 << 18 | 1 << 2 | 1 << 20),
            1 << 18 | 1 << 2 | 1 << 20
        );
    }
}

#[test]
fn the_render_ring_starts_and_resets_on_its_own_registers() {
    use crate::gtt::Layout;
    use crate::ring::{self, Ring};
    let mut bank = Bank::probed();
    let clock = Ticks::new();
    let fw = Forcewake::init(&mut bank, &clock, Timeouts::default()).unwrap();
    let layout = Layout::new(fake::FIRMWARE_RUN, fake::ENTRIES, 4, (256 << 20) / 4096).unwrap();
    let mut ring = Ring::new(4);
    {
        let mut awake = fw.hold(&mut bank, &clock).unwrap();
        ring::start(
            &mut awake,
            &clock,
            &Timeouts::default(),
            &regs::RENDER,
            &layout,
            &mut ring,
        )
        .unwrap();
        ring::reset(&mut awake, &clock, &Timeouts::default(), &regs::RENDER).unwrap();
    }
    assert_eq!(
        bank.get(0x4080),
        layout.render_status * 4096,
        "status page at 0x4080"
    );
    assert_eq!(bank.get(0x2038), layout.render_ring * 4096);
    assert_eq!(bank.get(0x2228), layout.pp_dir_base());
    assert_eq!(
        bank.get(0x229C) & (1 << 9 | 0x2800),
        1 << 9 | 0x2800,
        "PPGTT on, TLB bits kept"
    );
    assert_eq!(
        bank.writes_to(regs::GDRST),
        [1 << 1, 1 << 1],
        "the render domain, twice"
    );
    // Nothing of the blitter's touched.
    assert!(bank.writes_to(regs::bcs::CTL).is_empty());
}

#[test]
fn a_render_request_flushes_loads_its_settings_and_stores_its_number() {
    use crate::render;
    let mut out = [0u32; 64];
    let n = render::request(0x1000_0200, 0x1000_0100, 5, &[], &mut out).unwrap();
    assert_eq!(n, render::REQUEST_EXTRA);
    assert_eq!(n % 2, 0, "whole quadwords");
    assert_eq!(&out[..4], &render::stall());
    assert_eq!(&out[4..8], &render::flush_invalidate(0x1000_0200));
    assert_eq!(out[8], 0x22 << 23 | 5, "the context settings");
    assert_eq!(&out[15..21], &render::breadcrumb(0x1000_0100, 5));
    // An odd body is padded; too small an output is refused.
    let n = render::request(0, 0, 1, &[0, 0, 0], &mut out).unwrap();
    assert_eq!(n % 2, 0);
    assert_eq!(render::request(0, 0, 1, &[], &mut out[..10]), None);
}

#[test]
fn the_ring_emits_whole_quadwords_and_wraps() {
    use crate::ring::Ring;
    let mut mem = [0xAAAA_AAAAu32; 1024];
    let mut ring = Ring::new(1);
    assert_eq!(ring.emit(&mut mem, 0, &[1, 2, 3]), None, "odd");
    assert_eq!(ring.emit(&mut mem, 0, &[1, 2]), Some(8));
    assert_eq!(&mem[..3], &[1, 2, 0xAAAA_AAAA]);
    assert_eq!(
        ring.emit(&mut mem, 0, &[0; 1024]),
        None,
        "never the whole ring"
    );
}

/// igt's `gen7_gpgpu_fillfunc` for a 64 x 4 fill at the origin, with its
/// state 4 KiB-aligned at 0x10000: one thread a group, SIMD16, one CURBE
/// register.
fn igt_fill() -> aricode_abi::gpu::Dispatch {
    aricode_abi::gpu::Dispatch {
        general_base: 0,
        surface_base: 0x1_0000,
        dynamic_base: 0x1_0000,
        instruction_base: 0x1_0000,
        scratch: 0,
        max_threads: 1,
        urb_entries: 0,
        urb_entry_regs: 0,
        curbe_regs: 1,
        curbe_offset: 0x40,
        curbe_len: 32,
        descriptor_offset: 0x200,
        descriptors: 1,
        descriptor: 0,
        simd: 16,
        group: [1, 1, 1],
        start: [0, 0, 0],
        groups: [4, 4, 1],
        right_mask: 0xFFFF,
        bottom_mask: 0xFFFF_FFFF,
    }
}

#[test]
fn a_dispatch_is_igts_gpgpu_fill_between_linuxs_flushes() {
    extern crate std;
    use crate::compute;
    use crate::render::*;
    let (w, n) = compute::emit(&igt_fill(), 128).unwrap();
    let w = &w[..n];
    let invalidate = [
        PIPE_CONTROL,
        PC_STALL_AT_SCOREBOARD | PC_CS_STALL,
        0,
        0,
        PIPE_CONTROL,
        PC_STATE_INVALIDATE,
        0,
        0,
    ];
    let flush = [
        PIPE_CONTROL,
        PC_RENDER_TARGET_FLUSH | PC_DEPTH_FLUSH | PC_DC_FLUSH | PC_CS_STALL,
        0,
        0,
    ];
    let mut want: std::vec::Vec<u32> = std::vec::Vec::new();
    want.extend(invalidate);
    want.extend([0x6904_0002, 0]);
    want.extend(invalidate);
    want.extend(flush);
    // igt's `gen7_emit_state_base_address`, with the bases relocated.
    want.extend([0x6101_0008, 1, 0x1_0001, 0x1_0001, 0, 0x1_0001, 0, 1, 0, 1]);
    want.extend(invalidate);
    // `gen7_emit_vfe_state(THREADS 1, URB entries 0, URB size 0, CURBE 1,
    // GPGPU mode)`.
    want.extend([0x7000_0006, 0, 1 << 2, 0, 1, 0, 0, 0]);
    want.extend([0x7001_0002, 0, 32, 0x40]);
    want.extend([0x7002_0002, 0, 32, 0x200]);
    // `gen7_emit_gpgpu_walk(0, 0, 64, 4)`.
    want.extend([
        0x7105_0009,
        0,
        1 << 30,
        0,
        4,
        0,
        4,
        0,
        1,
        0xFFFF,
        0xFFFF_FFFF,
    ]);
    want.extend([0x7004_0000, 0]);
    assert_eq!(w, &want[..]);
    assert!(n <= compute::MAX_DWORDS);
    // Nothing that reaches registers: the gateway's MMIO bits, the walker's
    // indirect and predicate bits stay clear.
    let vfe = w
        .iter()
        .position(|&x| x == compute::MEDIA_VFE_STATE)
        .unwrap();
    assert_eq!(w[vfe + 2] & 3 << 3, 0);
    let walker = w.iter().position(|&x| x == compute::GPGPU_WALKER).unwrap();
    assert_eq!(w[walker] & (1 << 10 | 1 << 8), 0);
    // It fits a render request.
    let mut out = [0u32; 256];
    let r = request(0x80, 0x100, 7, w, &mut out).unwrap();
    // `REQUEST_EXTRA` counts the no-op that may pad to a quadword.
    assert_eq!(r, (n + REQUEST_EXTRA - 1).next_multiple_of(2));
}

#[test]
fn a_dispatch_carries_its_geometry_and_sizes() {
    use crate::compute;
    let mut d = igt_fill();
    d.max_threads = 128;
    d.urb_entries = 2;
    d.urb_entry_regs = 3;
    d.curbe_len = 0;
    d.scratch = 0x1400 | 2;
    d.descriptors = 4;
    d.descriptor = 3;
    d.simd = 8;
    d.group = [4, 2, 2];
    d.start = [1, 2, 3];
    d.groups = [10, 20, 30];
    let (w, n) = compute::emit(&d, 128).unwrap();
    let w = &w[..n];
    // No CURBE load without data.
    assert!(!w.contains(&compute::MEDIA_CURBE_LOAD));
    let vfe = w
        .iter()
        .position(|&x| x == compute::MEDIA_VFE_STATE)
        .unwrap();
    assert_eq!(
        &w[vfe + 1..vfe + 5],
        &[0x1402, 127 << 16 | 2 << 8 | 1 << 2, 0, 2 << 16 | 1]
    );
    let idl = w
        .iter()
        .position(|&x| x == compute::MEDIA_INTERFACE_DESCRIPTOR_LOAD)
        .unwrap();
    assert_eq!(w[idl + 2], 4 * 32);
    let walker = w.iter().position(|&x| x == compute::GPGPU_WALKER).unwrap();
    assert_eq!(
        &w[walker + 1..walker + 9],
        &[3, 1 << 16 | 1 << 8 | 3, 1, 10, 2, 20, 3, 30]
    );
    // GT1 has 36 threads.
    assert_eq!(compute::threads(0x0156), Some(36));
    assert_eq!(compute::threads(0x0166), Some(128));
    assert_eq!(compute::emit(&d, 36), Err(compute::Bad::Threads));
}

#[test]
fn a_dispatch_the_hardware_would_not_take_is_refused() {
    use crate::compute::{validate, Bad};
    let ok = igt_fill();
    assert_eq!(validate(&ok, 128), Ok(()));
    let cases: [(fn(&mut aricode_abi::gpu::Dispatch), Bad); 18] = [
        (|d| d.surface_base = 0x1_0800, Bad::Base),
        (|d| d.general_base = 1, Bad::Base),
        (|d| d.scratch = 0x10, Bad::Scratch),
        (|d| d.scratch = 12, Bad::Scratch),
        (|d| d.max_threads = 0, Bad::Threads),
        (|d| d.max_threads = 129, Bad::Threads),
        (|d| d.urb_entries = 65, Bad::Urb),
        (|d| d.urb_entries = 1, Bad::Urb),
        (
            |d| {
                d.urb_entries = 64;
                d.urb_entry_regs = 32;
            },
            Bad::Urb,
        ),
        (|d| d.curbe_len = 64, Bad::Curbe),
        (|d| d.curbe_offset = 0x50, Bad::Curbe),
        (|d| d.descriptors = 0, Bad::Descriptor),
        (|d| d.descriptor = 1, Bad::Descriptor),
        (|d| d.descriptor_offset = 0x210, Bad::Descriptor),
        (|d| d.simd = 4, Bad::Simd),
        (|d| d.group = [8, 8, 2], Bad::Group),
        (
            |d| {
                d.simd = 32;
                d.group = [8, 8, 1];
            },
            Bad::Group,
        ),
        (|d| d.start = [4, 0, 0], Bad::Groups),
    ];
    for (i, (change, bad)) in cases.iter().enumerate() {
        let mut d = ok;
        change(&mut d);
        assert_eq!(validate(&d, 128), Err(*bad), "case {i}");
    }
    let mut d = ok;
    d.groups[2] = 0;
    assert_eq!(validate(&d, 128), Err(Bad::Groups));
    let mut d = ok;
    d.right_mask = 0;
    assert_eq!(validate(&d, 128), Err(Bad::Groups));
    let mut d = ok;
    d.group = [0, 1, 1];
    assert_eq!(validate(&d, 128), Err(Bad::Group));
}

#[test]
fn descriptors_and_surfaces_are_igts() {
    use crate::compute::{surface_2d, Descriptor, FORMAT_R8_UNORM};
    // igt's `gen7_fill_interface_descriptor`: the kernel at 0x100, the
    // binding table at 0xC0, one CURBE register.
    let d = Descriptor {
        kernel: 0x100,
        binding_table: 0xC0,
        binding_entries: 0,
        curbe_regs: 1,
        slm_blocks: 0,
        barrier: false,
    };
    assert_eq!(
        d.encode(),
        Some([0x100, 1 << 18, 0, 0xC0, 1 << 16, 0, 0, 0])
    );
    let slm = Descriptor {
        slm_blocks: 16,
        barrier: true,
        ..d
    };
    assert_eq!(slm.encode().unwrap()[5], 1 << 21 | 16 << 16);
    for bad in [
        Descriptor { kernel: 0x120, ..d },
        Descriptor {
            binding_table: 0xC8,
            ..d
        },
        Descriptor {
            binding_table: 0x1_0000,
            ..d
        },
        Descriptor {
            binding_entries: 32,
            ..d
        },
        Descriptor { slm_blocks: 3, ..d },
    ] {
        assert_eq!(bad.encode(), None, "{bad:?}");
    }
    // igt's `gen7_fill_surface_state` for a linear 64 x 4 R8 buffer
    // (without the Haswell channel selects igt also sets).
    assert_eq!(
        surface_2d(FORMAT_R8_UNORM, 0x2_0000, 64, 4, 64),
        Some([
            1 << 29 | 0x140 << 18 | 1 << 8,
            0x2_0000,
            3 << 16 | 63,
            63,
            0,
            0,
            0,
            0
        ])
    );
    assert_eq!(surface_2d(FORMAT_R8_UNORM, 0, 0, 4, 64), None);
    assert_eq!(surface_2d(FORMAT_R8_UNORM, 0, 64, 4, 1 << 18 | 1), None);
}

#[test]
fn the_residue_clear_fills_every_slot_of_a_gt2() {
    use crate::clear;
    use crate::compute;
    let mut code = [[0u32; 4]; 192];
    let n = clear::program(&mut code).unwrap();
    // g127 to keep the header, the slot, 63 zeroings, the sweep, the mark,
    // the last zeroings and the end.
    assert!(n < 192, "{n} instructions");
    let last = code[n - 1];
    assert_eq!(last[0] & 0x7F, 0x31, "a send ends it");
    assert_ne!(last[3] & 1 << 31, 0, "with the end of thread");
    let mut state = [0u32; 1024];
    let base = 0x1_0000;
    let d = clear::prepare(&mut state, base, 7, 128).unwrap();
    // A half-slice's 64 threads a group, one group a half-slice.
    assert_eq!(d.group, [64, 1, 1]);
    assert_eq!(d.groups, [2, 1, 1]);
    assert_eq!(d.max_threads, 128);
    assert_eq!(compute::validate(&d, 128), Ok(()));
    // The program, then the two surfaces, the sweep's and the marks'.
    assert_eq!(&state[..4], &code[0]);
    assert_eq!(state[0xC00 / 4..0xC00 / 4 + 2], [0xC40, 0xC80]);
    assert_eq!(state[0xC40 / 4 + 1], base + 2 * 4096);
    assert_eq!(state[0xC80 / 4 + 1], base + 4096);
    assert_eq!(state[0xD40 / 4], 7, "the clear's number in the CURBE");
    // GT1: one group of 36.
    let d = clear::prepare(&mut state, base, 1, 36).unwrap();
    assert_eq!((d.group, d.groups), ([36, 1, 1], [1, 1, 1]));
    assert_eq!(clear::prepare(&mut state, base, 1, 130), None);
    assert_eq!(clear::groups(0), None);
    assert_eq!(clear::groups(127), None);
    // Counting: only this clear's number counts.
    let mut marks = [0u32; 1024];
    marks[0] = 7;
    marks[9] = 7;
    marks[10] = 6;
    marks[300] = 7; // past the 256 slots
    assert_eq!(clear::covered(&marks, 7), 2);
    assert!(clear::slm_on(1) && !clear::slm_on(0x0008_0040));
    assert_eq!(clear::PAGES, 34);
}
