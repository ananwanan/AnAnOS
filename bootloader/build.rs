fn main() {
    println!("cargo:rustc-link-arg=-Tbootloader/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
}
