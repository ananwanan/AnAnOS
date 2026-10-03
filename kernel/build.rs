fn main() {
    // Host tests exercise the no_std library without the bare-metal linker script.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        let script = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../boot/linker.ld");
        println!("cargo:rustc-link-arg-bin=kernel=-T{}", script.display());
    }
    println!("cargo:rerun-if-changed=../boot/linker.ld");
}
