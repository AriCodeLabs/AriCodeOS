//! `fs_victim`: writes a secret into its private directory and exits without
//! closing the descriptor (regression for audit e9fdf38, finding 1). It also
//! replaces its file with a synthetic string and leaves a chunked replace
//! half done on exit (audit 6ee19fb, findings 1 and 3): none of that may
//! reach another app or block its replaces.
#![no_std]
#![no_main]

use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions};

fn main(_arg: u64) -> u64 {
    let (mut fs_ep, mut hw) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            fs_ep = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let (Some(ep), Some(hw)) = (fs_ep, hw) else {
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let fd = match fs.open("secreto").or_else(|_| fs.create("secreto")) {
        Ok(fd) => fd,
        Err(_) => return 3,
    };
    if fs.write(fd, 0, b"VICTIM_SECRET").is_err() {
        return 4;
    }
    // An atomic replace leaves its bytes in fsd's shared journal.
    if fs.replace(fd, b"XYNTHETIC_PRIVATE_DATA").is_err() {
        return 5;
    }
    // And a chunked replace that is never committed: the owner goes away.
    if fs.replace_chunk(fd, 0, b"abandonado").is_err() {
        return 6;
    }
    // No `close`: the descriptor and the transaction stay alive in fsd.
    sys::print(b"fs_victim: secret written, descriptor left open on exit\n");
    0
}

aricode_rt::entry!(main);
