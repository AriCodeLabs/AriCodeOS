// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A test package named `home` that asks for a private directory, which
//! would be the owner's folder
//! (ADR 0074 phase C): the broker denies the launch, so this never runs.
#![no_std]
#![no_main]

fn main(_arg: u64) -> u64 {
    aricode_rt::sys::print(b"home: started with what it asked for\n");
    1
}

aricode_rt::entry!(main);
