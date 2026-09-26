fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=-T{dir}/kernel.ld");
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=0x1000");
    println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-changed=src/arch/x86_64/stubs.s");
    // Programas de usuario embebidos: los construye `cargo xtask` antes que el kernel.
    println!("cargo:rerun-if-env-changed=ARICODE_USER_BIN");
    match std::env::var("ARICODE_USER_BIN") {
        Ok(dir) => println!("cargo:rerun-if-changed={dir}"),
        Err(_) => panic!(
            "ARICODE_USER_BIN no definido: el kernel embebe los programas de usuario de \
             user/testprogs. Compila con `cargo xtask build|test|check`, que los construye y \
             exporta la variable."
        ),
    }
}
