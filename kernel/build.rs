//! Passes the kernel linker script to the linker.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    println!("cargo:rerun-if-changed=link.ld");
    println!("cargo:rustc-link-arg-bins=-T{dir}/link.ld");
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=4096");
    println!("cargo:rustc-link-arg-bins=--build-id=none");
}
