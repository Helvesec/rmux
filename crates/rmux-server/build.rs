use std::fs;
use std::path::Path;

fn main() {
    let presets_dir = Path::new("tunnels");
    println!("cargo:rerun-if-changed=tunnels");

    let mut presets = Vec::new();
    if presets_dir.is_dir() {
        for entry in fs::read_dir(presets_dir).expect("read tunnel preset directory") {
            let path = entry.expect("read tunnel preset entry").path();
            if path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                println!("cargo:rerun-if-changed={}", path.display());
                let name = path
                    .file_stem()
                    .expect("tunnel preset file has a stem")
                    .to_string_lossy()
                    .into_owned();
                let content = fs::read_to_string(&path).expect("read tunnel preset");
                presets.push((name, content));
            }
        }
    }
    presets.sort_by(|left, right| left.0.cmp(&right.0));

    let mut generated =
        String::from("pub(super) const SHIPPED_TUNNEL_PRESETS: &[(&str, &str)] = &[\n");
    for (name, content) in presets {
        generated.push_str(&format!("    ({name:?}, {content:?}),\n"));
    }
    generated.push_str("];\n");

    let out_path =
        Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR is set")).join("tunnel_presets.rs");
    fs::write(out_path, generated).expect("write tunnel preset include");
}
