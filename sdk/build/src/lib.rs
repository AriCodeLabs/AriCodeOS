// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! A user program's `build.rs` only has to call [`user_program`]: it links
//! the binary with `sdk/rust/user.ld` (static ELF at `0x400000`) and relinks
//! if the script changes.

/// Path of the user program linker script, fixed when this crate is compiled
/// (it lives with the SDK, not with each program).
pub const USER_LD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rust/user.ld");

/// Emits the cargo directives for a user program.
pub fn user_program() {
    println!("cargo:rustc-link-arg-bins=-T{USER_LD}");
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=0x1000");
    println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
    println!("cargo:rerun-if-changed={USER_LD}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn linker_script_exists() {
        assert!(
            std::path::Path::new(super::USER_LD).is_file(),
            "{}",
            super::USER_LD
        );
    }
}
