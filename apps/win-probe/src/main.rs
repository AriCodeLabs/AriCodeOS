// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! `win_probe`: if it ever runs (boot with `gui`), it opens a window and
//! exits. Its role in the tests is to be denied by the broker without a GUI.
#![no_std]
#![no_main]

use aricode_rt::sys::window::Window;
use aricode_rt::sys::{self, kind, permissions};

fn main(_arg: u64) -> u64 {
    let (mut comp, mut hw) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            comp = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let (Some(comp), Some(hw)) = (comp, hw) else {
        return 1;
    };
    match Window::create(comp, hw, 200, 100, "Probe") {
        Ok(w) => {
            let _ = w.present();
            let _ = w.close();
            sys::print(b"win_probe: window created and closed\n");
            0
        }
        Err(_) => 2,
    }
}

aricode_rt::entry!(main);
