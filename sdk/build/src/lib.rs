//! Un `build.rs` de programa de usuario solo tiene que llamar a
//! [`user_program`]: enlaza el binario con `sdk/rust/user.ld` (ELF estático
//! en `0x400000`) y vuelve a enlazar si el script cambia.

/// Ruta del linker script de los programas de usuario, fijada al compilar
/// esta crate (vive junto al SDK, no junto a cada programa).
pub const USER_LD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rust/user.ld");

/// Emite las directivas de cargo para un programa de usuario.
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
