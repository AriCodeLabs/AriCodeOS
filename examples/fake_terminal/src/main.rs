// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Asks for `terminal` under another name (ADR 0074 phase C): the broker
//! denies the launch, so this never runs.
#![no_std]
#![no_main]

fn main(_arg: u64) -> u64 {
    aricode_rt::sys::print(b"fake_terminal: started with the terminal capability\n");
    1
}

aricode_rt::entry!(main);
