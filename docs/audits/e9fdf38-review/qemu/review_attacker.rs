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
    let mut b = [0u8; 32];
    for fd in 0..16 {
        if let Ok(n) = fs.read(fd, 0, &mut b) {
            if &b[..n] == b"VICTIM_SECRET" {
                sys::print(b"REVIEW_CROSS_NAMESPACE_READ: ");
                sys::print(&b[..n]);
                sys::print(b"\n");
                return 1;
            }
        }
    }
    sys::print(b"REVIEW_PRIVATE_FDS_ISOLATED\n");
    0
}
aricode_rt::entry!(main);
