fn main() {
    const WINDOWS_MANIFEST: &str = "resources/windows/rmux.exe.manifest";

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest_file(WINDOWS_MANIFEST)
            .expect("unable to embed Windows application manifest");
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={WINDOWS_MANIFEST}");
}
