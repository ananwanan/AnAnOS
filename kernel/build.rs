fn main() {
    println!("cargo:rustc-link-arg=-Tboot/linker.ld");
    println!("cargo:rerun-if-changed=../boot/linker.ld");
}
