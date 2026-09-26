fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=-T{dir}/kernel.ld");
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=0x1000");
    println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
    println!("cargo:rerun-if-changed=kernel.ld");
    println!("cargo:rerun-if-changed=src/arch/x86_64/stubs.s");
    println!("cargo:rerun-if-changed=src/user/programs.s");
}
