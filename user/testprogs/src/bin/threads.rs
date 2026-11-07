// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Threads in a process (ADR 0082), from ring 3: threads that compute and are
//! joined, each with its own floating point state; a futex ping-pong; a pool's
//! rounds; the thread limit; join's refusals. Exits with 21 when all hold; any
//! other code is the step that failed.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;
use aricode_rt::sys::{self, Error, TIMEOUT_INFINITE};
use aricode_rt::thread::{self, Pool};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

fn say(text: &str) {
    sys::print(b"threads: ");
    sys::print(text.as_bytes());
    sys::print(b"\n");
}

/// A sum in floating point that depends on `seed`: a thread whose x87/SSE state
/// leaked into another's would get another number.
fn float_work(seed: u64) -> u64 {
    let mut x = seed as f64;
    for i in 1..20_000u64 {
        x = x * 0.999_9 + (i % 7) as f64 / (seed as f64 + 1.0);
    }
    x as u64
}

fn computed() -> Result<(), u64> {
    let expected: Vec<u64> = (1..=3).map(float_work).collect();
    let handles: Vec<_> = (1..=3u64)
        .map(|seed| thread::spawn(move || float_work(seed)))
        .collect::<Result<_, _>>()
        .map_err(|_| 2u64)?;
    let mine = float_work(4);
    for (h, want) in handles.into_iter().zip(expected) {
        if h.join() != Ok(want) {
            return Err(3);
        }
    }
    if mine != float_work(4) {
        return Err(4);
    }
    say("three threads computed and joined");
    Ok(())
}

/// Two threads take turns through a futex: the word says whose turn it is.
fn ping_pong() -> Result<(), u64> {
    const TURNS: u32 = 1000;
    let word = Arc::new(AtomicU32::new(0));
    let other = word.clone();
    // The other thread plays the odd turns.
    let player = thread::spawn(move || {
        let mut played = 0;
        loop {
            let now = other.load(Ordering::Acquire);
            if now >= TURNS {
                return played;
            }
            if now % 2 == 1 {
                other.store(now + 1, Ordering::Release);
                sys::futex_wake(&other, 1);
                played += 1;
            } else {
                let _ = sys::futex_wait(&other, now, TIMEOUT_INFINITE);
            }
        }
    })
    .map_err(|_| 5u64)?;
    let mut played = 0;
    loop {
        let now = word.load(Ordering::Acquire);
        if now >= TURNS {
            break;
        }
        if now.is_multiple_of(2) {
            word.store(now + 1, Ordering::Release);
            sys::futex_wake(&word, 1);
            played += 1;
        } else {
            let _ = sys::futex_wait(&word, now, TIMEOUT_INFINITE);
        }
    }
    let theirs = player.join().map_err(|_| 6u64)?;
    if played + theirs != TURNS as u64 || word.load(Ordering::Acquire) != TURNS {
        return Err(7);
    }
    say("a futex ping-pong of 1000 turns");
    Ok(())
}

fn pool_rounds() -> Result<(), u64> {
    let pool = Pool::new(3);
    if pool.size() != 4 {
        return Err(8);
    }
    let cells: Vec<AtomicU64> = (0..4).map(|_| AtomicU64::new(0)).collect();
    for round in 1..=200u64 {
        pool.run(4, &|part| {
            cells[part].fetch_add(round * (part as u64 + 1), Ordering::Relaxed);
        });
        // Every part is done when `run` returns.
        let want = round * (round + 1) / 2;
        for (part, c) in cells.iter().enumerate() {
            if c.load(Ordering::Relaxed) != want * (part as u64 + 1) {
                return Err(9);
            }
        }
    }
    drop(pool);
    say("a pool of 4 ran 200 rounds");
    Ok(())
}

/// Threads up to the default limit of 8, the main one counted, then `Limit`.
fn limit() -> Result<(), u64> {
    let gate = Arc::new(AtomicU32::new(0));
    let mut held = Vec::new();
    loop {
        let g = gate.clone();
        match thread::spawn(move || {
            while g.load(Ordering::Acquire) == 0 {
                let _ = sys::futex_wait(&g, 0, TIMEOUT_INFINITE);
            }
            0
        }) {
            Ok(h) => held.push(h),
            Err(Error::Limit) => break,
            Err(_) => return Err(10),
        }
    }
    if held.len() != 7 {
        return Err(11);
    }
    gate.store(1, Ordering::Release);
    sys::futex_wake(&gate, u32::MAX);
    for h in held {
        h.join().map_err(|_| 12u64)?;
    }
    // A joined thread's place is free again.
    let again = thread::spawn(|| 5).map_err(|_| 13u64)?;
    if again.join() != Ok(5) {
        return Err(14);
    }
    say("the limit of 8 threads holds, and joining frees a place");
    Ok(())
}

fn refusals() -> Result<(), u64> {
    let main = sys::object_id(sys::Handle(sys::handle::MAIN_THREAD)).map_err(|_| 15u64)?;
    if sys::thread_join(main, 0) != Err(Error::Invalid) {
        return Err(16);
    }
    if sys::thread_join(999_999, 0) != Err(Error::NotFound) {
        return Err(17);
    }
    let t = thread::spawn(|| 1).map_err(|_| 18u64)?;
    let tid = t.tid();
    t.join().map_err(|_| 19u64)?;
    if sys::thread_join(tid, 0) != Err(Error::NotFound) {
        return Err(20);
    }
    let word = AtomicU32::new(7);
    // A word that does not hold the value: no wait at all.
    if sys::futex_wait(&word, 8, TIMEOUT_INFINITE).is_err() {
        return Err(22);
    }
    // One that does, with a deadline and nobody to wake it.
    if sys::futex_wait(&word, 7, 30) != Err(Error::Timeout) {
        return Err(23);
    }
    say("join and futex refuse what they should");
    Ok(())
}

fn main(_arg: u64) -> u64 {
    let steps: [fn() -> Result<(), u64>; 5] = [computed, ping_pong, pool_rounds, limit, refusals];
    for step in steps {
        if let Err(code) = step() {
            return code;
        }
    }
    21
}

aricode_rt::entry!(main);
