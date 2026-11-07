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
