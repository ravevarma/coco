//! Selects the linker script for the target architecture.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let script = match arch.as_str() {
        "riscv64" => "link-riscv64.ld",
        "riscv32" => "link-riscv32.ld",
        "aarch64" => "link-aarch64.ld",
        "x86" => "link-x86.ld",
        _ => return, // host builds (e.g. `cargo check` on x86_64) do not link
    };
    println!("cargo:rerun-if-changed={script}");
    println!("cargo:rerun-if-changed=link-common.ld");
    println!("cargo:rustc-link-arg-bins=-T{dir}/{script}");
    println!("cargo:rustc-link-arg-bins=-L{dir}");
    if arch == "x86" {
        // The hosted i686 target is used freestanding and links through the
        // C compiler driver: no C runtime or libc, static, no build ID.
        println!("cargo:rustc-link-arg-bins=-nostdlib");
        println!("cargo:rustc-link-arg-bins=-static");
        println!("cargo:rustc-link-arg-bins=-Wl,--build-id=none");
        println!("cargo:rustc-link-arg-bins=-Wl,-z,max-page-size=4096");
    } else {
        println!("cargo:rustc-link-arg-bins=--build-id=none");
        // Every port uses 4 KiB pages; lld's AArch64 default (64 KiB) pads
        // the file by ~70 KB.
        println!("cargo:rustc-link-arg-bins=-zmax-page-size=4096");
    }
}
