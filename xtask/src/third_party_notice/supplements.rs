//! Exact upstream texts omitted from published crate archives.
//! This lookup never selects a package; it only supplies texts for an observed
//! artifact whose registry, version, repository and published VCS identity match.

use std::path::Path;

use serde_json::Value;

const HAX: &str = "d8b5b3d3b666fee8943a351445d2b680105e8ea3";
const LIBCRUX: &str = "c5fb80f37530ee9b2df9501ae5ff8cb4a973a4bd";
const PLATFORM: &str = "316fcd6c19c2e33bd0429e1319b6557d91038512";
const HAX_LICENSE: &str =
    include_str!("../../../scripts/packaging/licenses/hax-d8b5b3d3b666/LICENSE");
const LIBCRUX_LICENSE: &str =
    include_str!("../../../scripts/packaging/licenses/libcrux-c5fb80f37530/LICENSE");
const LIBCRUX_MIT: &str =
    include_str!("../../../scripts/packaging/licenses/libcrux-c5fb80f37530/LICENSE-MIT");
const PLATFORM_LICENSE: &str =
    include_str!("../../../scripts/packaging/licenses/libcrux-316fcd6c19c2/LICENSE");
const PLATFORM_MIT: &str =
    include_str!("../../../scripts/packaging/licenses/libcrux-316fcd6c19c2/LICENSE-MIT");

type LicenseTexts = Vec<(String, String)>;

pub(super) fn texts(package: &Value, directory: &Path) -> Result<Option<LicenseTexts>, String> {
    let name = super::text(package, "name")?;
    let version = super::text(package, "version")?;
    let (repository, revision, path, files): (_, _, _, &[(&str, &str)]) = match (name, version) {
        ("hax-lib", "0.3.7") => ("hacspec/hax", HAX, "hax-lib", &[("LICENSE", HAX_LICENSE)]),
        ("hax-lib-macros", "0.3.7") => (
            "hacspec/hax",
            HAX,
            "hax-lib/macros",
            &[("LICENSE", HAX_LICENSE)],
        ),
        ("hax-lib-macros-types", "0.3.7") => (
            "hacspec/hax",
            HAX,
            "hax-lib/macros/types",
            &[("LICENSE", HAX_LICENSE)],
        ),
        ("libcrux-platform", "0.0.3") => (
            "cryspen/libcrux",
            PLATFORM,
            "crates/sys/platform",
            &[("LICENSE", PLATFORM_LICENSE), ("LICENSE-MIT", PLATFORM_MIT)],
        ),
        ("libcrux-intrinsics", "0.0.8")
        | ("libcrux-ml-kem", "0.0.10")
        | ("libcrux-secrets", "0.0.6")
        | ("libcrux-sha3", "0.0.10")
        | ("libcrux-traits", "0.0.8") => (
            "cryspen/libcrux",
            LIBCRUX,
            match name {
                "libcrux-intrinsics" => "crates/utils/intrinsics",
                "libcrux-ml-kem" => "libcrux-ml-kem",
                "libcrux-secrets" => "crates/utils/secrets",
                "libcrux-sha3" => "crates/algorithms/sha3",
                _ => "traits",
            },
            &[("LICENSE", LIBCRUX_LICENSE), ("LICENSE-MIT", LIBCRUX_MIT)],
        ),
        _ => return Ok(None),
    };
    let vcs = super::json(&super::read(&directory.join(".cargo_vcs_info.json"))?)?;
    if package["source"].as_str() != Some("registry+https://github.com/rust-lang/crates.io-index")
        || package["repository"].as_str()
            != Some(format!("https://github.com/{repository}").as_str())
        || package["license"].as_str() != Some("Apache-2.0")
        || vcs["git"]["sha1"].as_str() != Some(revision)
        || vcs["path_in_vcs"].as_str() != Some(path)
    {
        return Err(format!(
            "upstream license supplement identity mismatch: {name} {version}"
        ));
    }
    Ok(Some(
        files
            .iter()
            .map(|(file, contents)| {
                (
                    format!("https://raw.githubusercontent.com/{repository}/{revision}/{file}"),
                    (*contents).to_owned(),
                )
            })
            .collect(),
    ))
}
