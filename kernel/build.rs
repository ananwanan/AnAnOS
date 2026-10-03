fn main() {
    // Host unit tests exercise pure memory algorithms, without the ARM linker script.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        println!("cargo:rustc-link-arg=-Tboot/linker.ld");
    }
    println!("cargo:rerun-if-changed=../boot/linker.ld");
}
