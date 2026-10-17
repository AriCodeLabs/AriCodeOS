// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
#![no_std]
#![no_main]
use aricode_rt::sys::{self, fs::FsClient, kind, permissions};
fn main(_: u64) -> u64 {
    let (mut ep, mut hw) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            ep = Some(h);
        }
        if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let mut fs = FsClient::new(ep.unwrap(), hw.unwrap()).unwrap();
    let fd = fs.create("secret.txt").unwrap();
    fs.write(fd, 0, b"VICTIM_SECRET").unwrap();
    sys::print(b"REVIEW_VICTIM_WROTE_PRIVATE_FILE\n");
    // Exit without close: the descriptor must not be usable by another app.
    0
}
aricode_rt::entry!(main);
