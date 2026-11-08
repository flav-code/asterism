// The texts in locales/*.json are built into the crate.
fn main() {
    println!("cargo:rerun-if-changed=../../locales");
}
