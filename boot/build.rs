//! Linker flags for a deterministic PE image: `/Brepro` replaces the COFF
//! timestamp with a content hash and `/DEBUG:NONE` drops the PDB debug
//! directory (whose path and signature vary between builds).
fn main() {
    println!("cargo:rustc-link-arg-bins=/Brepro");
    println!("cargo:rustc-link-arg-bins=/DEBUG:NONE");
}
