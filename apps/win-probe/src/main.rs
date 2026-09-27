//! `win_probe`: si llega a ejecutarse (arranque con `gui`), abre una ventana
//! y sale. Su papel en los tests es que el broker lo deniegue sin GUI.
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
    match Window::create(comp, hw, 200, 100, "Sonda") {
        Ok(w) => {
            let _ = w.present();
            let _ = w.close();
            sys::print(b"win_probe: ventana creada y cerrada\n");
            0
        }
        Err(_) => 2,
    }
}

aricode_rt::entry!(main);
