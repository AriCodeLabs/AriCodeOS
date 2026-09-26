//! Programas de usuario: se buscan por nombre en el initrd.

pub fn by_name(name: &str) -> Option<&'static [u8]> {
    crate::initrd::find(name)
}
