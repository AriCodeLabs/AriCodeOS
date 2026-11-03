// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `hello_more`: a second application built outside the tree, so that a
//! profile of the tests (ADR 0074) installs, and removes, more than one
//! package. It says its version and exits.
#![no_std]
#![no_main]

use aricode_rt::sys;

/// Version set by xtask when packaging (`APP_VERSION`); "dev" when built by
/// hand.
const VERSION: &str = match option_env!("APP_VERSION") {
    Some(v) => v,
    None => "dev",
};

fn main(arg: u64) -> u64 {
    let mut b = [0u8; 20];
    sys::print(b"hello_more v");
    sys::print(VERSION.as_bytes());
    sys::print(b": the second app of the profile (arg ");
    sys::print(aricode_rt::fmt_u64(arg, &mut b));
    sys::print(b")\n");
    0
}

aricode_rt::entry!(main);
