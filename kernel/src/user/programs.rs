//! User programs: looked up by name in the initrd.

pub fn by_name(name: &str) -> Option<&'static [u8]> {
    crate::initrd::find(name)
}
