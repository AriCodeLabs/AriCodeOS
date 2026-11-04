// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A test package that asks for the settings endpoint without being the
//! settings app
//! (ADR 0074 phase C): the broker denies the launch, so this never runs.
#![no_std]
#![no_main]

fn main(_arg: u64) -> u64 {
    aricode_rt::sys::print(b"fake_settings: started with what it asked for\n");
    1
}

aricode_rt::entry!(main);
