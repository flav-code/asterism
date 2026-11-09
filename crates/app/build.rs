// The texts are built into the binary, and on Windows the logo goes into the exe (see asterism.rc).
fn main() {
    println!("cargo:rerun-if-changed=../../locales");
    println!("cargo:rerun-if-changed=asterism.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    embed_resource::compile("asterism.rc", embed_resource::NONE).manifest_optional().unwrap();
}
