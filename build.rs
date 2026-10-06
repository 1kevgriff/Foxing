fn main() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("foxing.manifest");
    println!("cargo:rerun-if-changed=foxing.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
