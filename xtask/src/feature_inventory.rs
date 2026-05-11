//! Feature inventory validation for the v0.0.5/v1 readiness.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const INVENTORY_PATH: &str = "docs/feature-inventory-v1.yaml";
const SDK_SRC_ROOT: &str = "crates/rmux-sdk/src";
const MAX_SDK_FILE_LINES: usize = 600;

const REQUIRED_FIELDS: &[&str] = &[
    "name",
    "api_item",
    "crate_module",
    "unit_test",
    "integration_test",
    "smoke_acceptance",
    "example_rustdoc",
    "linux",
    "macos",
    "windows",
    "result",
    "owner",
    "notes",
];

/// Feature-inventory subcommand mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Validate the YAML schema and feature verification references.
    Check,
    /// Validate SDK source file-size tracking rows.
    CheckFileSizes,
    /// Render a Markdown table from the YAML inventory.
    RenderMarkdown,
}

/// Error returned by feature-inventory validation.
#[derive(Debug)]
pub(crate) enum Error {
    /// Filesystem error while reading inventory or walking SDK source files.
    Io { path: PathBuf, source: io::Error },
    /// Schema or semantic validation error.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "feature inventory I/O error for {}: {source}",
                    path.display()
                )
            }
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

/// Runs feature-inventory validation or rendering.
pub(crate) fn run(mode: Mode, repo_root: &Path) -> Result<Option<String>, Error> {
    let inventory = read_inventory(repo_root)?;
    match mode {
        Mode::Check => {
            check_inventory(repo_root, &inventory)?;
            Ok(None)
        }
        Mode::CheckFileSizes => {
            check_file_sizes(repo_root, &inventory)?;
            Ok(None)
        }
        Mode::RenderMarkdown => Ok(Some(render_markdown(&inventory))),
    }
}

fn read_inventory(repo_root: &Path) -> Result<Inventory, Error> {
    let path = repo_root.join(INVENTORY_PATH);
    let text = fs::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    parse_inventory(&text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Inventory {
    version: u32,
    features: Vec<Feature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Feature {
    fields: HashMap<String, String>,
    deferral: HashMap<String, String>,
}

impl Feature {
    fn field(&self, key: &str) -> &str {
        self.fields.get(key).map(String::as_str).unwrap_or("")
    }
}

fn parse_inventory(text: &str) -> Result<Inventory, Error> {
    let mut version = None;
    let mut features = Vec::<Feature>::new();
    let mut current: Option<Feature> = None;
    let mut in_deferral = false;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("version:") {
            version = Some(parse_u32_value(value, line_number)?);
            continue;
        }
        if trimmed == "features:" {
            continue;
        }

        if let Some(rest) = line.strip_prefix("  - ") {
            if let Some(feature) = current.take() {
                features.push(feature);
            }
            let mut feature = Feature {
                fields: HashMap::new(),
                deferral: HashMap::new(),
            };
            let (key, value) = parse_key_value(rest, line_number)?;
            feature.fields.insert(key, value);
            current = Some(feature);
            in_deferral = false;
            continue;
        }

        if line.starts_with("    deferral:") {
            ensure_current(&current, line_number)?;
            in_deferral = true;
            continue;
        }

        if let Some(rest) = line.strip_prefix("      ") {
            if !in_deferral {
                return Err(Error::Invalid(format!(
                    "line {line_number}: nested field without `deferral:`"
                )));
            }
            let (key, value) = parse_key_value(rest, line_number)?;
            let feature = current.as_mut().expect("current feature checked");
            feature.deferral.insert(key, value);
            continue;
        }

        if let Some(rest) = line.strip_prefix("    ") {
            let (key, value) = parse_key_value(rest, line_number)?;
            let feature = current.as_mut().ok_or_else(|| {
                Error::Invalid(format!("line {line_number}: field outside a feature row"))
            })?;
            feature.fields.insert(key, value);
            in_deferral = false;
            continue;
        }

        return Err(Error::Invalid(format!(
            "line {line_number}: unsupported inventory syntax: {line}"
        )));
    }

    if let Some(feature) = current {
        features.push(feature);
    }

    Ok(Inventory {
        version: version.ok_or_else(|| Error::Invalid("missing `version`".to_owned()))?,
        features,
    })
}

fn ensure_current(current: &Option<Feature>, line_number: usize) -> Result<(), Error> {
    if current.is_some() {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "line {line_number}: `deferral` outside a feature row"
        )))
    }
}

fn parse_u32_value(value: &str, line_number: usize) -> Result<u32, Error> {
    value.trim().parse::<u32>().map_err(|error| {
        Error::Invalid(format!(
            "line {line_number}: invalid numeric value `{}`: {error}",
            value.trim()
        ))
    })
}

fn parse_key_value(text: &str, line_number: usize) -> Result<(String, String), Error> {
    let Some((key, value)) = text.split_once(':') else {
        return Err(Error::Invalid(format!(
            "line {line_number}: expected `key: value`"
        )));
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(Error::Invalid(format!("line {line_number}: empty key")));
    }
    Ok((key.to_owned(), parse_scalar(value.trim())))
}

fn parse_scalar(value: &str) -> String {
    let Some(stripped) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value.to_owned();
    };
    stripped
        .replace(r#"\""#, "\"")
        .replace(r"\n", "\n")
        .replace(r"\t", "\t")
}

fn check_inventory(repo_root: &Path, inventory: &Inventory) -> Result<(), Error> {
    if inventory.version != 1 {
        return Err(Error::Invalid(format!(
            "expected inventory version 1, got {}",
            inventory.version
        )));
    }
    if inventory.features.is_empty() {
        return Err(Error::Invalid("inventory has no features".to_owned()));
    }

    for feature in &inventory.features {
        for field in REQUIRED_FIELDS {
            if !feature.fields.contains_key(*field) {
                return Err(Error::Invalid(format!(
                    "feature `{}` is missing required field `{field}`",
                    feature.field("name")
                )));
            }
        }
        match feature.field("result") {
            "VERIFIED" => ensure_verified_reference(repo_root, feature)?,
            "DEFERRED" => ensure_deferred(feature)?,
            "UNVERIFIED" => {
                return Err(Error::Invalid(format!(
                    "feature `{}` is UNVERIFIED",
                    feature.field("name")
                )))
            }
            other => {
                return Err(Error::Invalid(format!(
                    "feature `{}` has invalid result `{other}`",
                    feature.field("name")
                )))
            }
        }
    }

    Ok(())
}

fn ensure_verified_reference(repo_root: &Path, feature: &Feature) -> Result<(), Error> {
    for field in [
        "example_rustdoc",
        "unit_test",
        "integration_test",
        "smoke_acceptance",
    ] {
        if field_contains_existing_path(repo_root, feature.field(field)) {
            return Ok(());
        }
    }
    Err(Error::Invalid(format!(
        "VERIFIED feature `{}` does not cite an existing test or example path",
        feature.field("name")
    )))
}

fn field_contains_existing_path(repo_root: &Path, value: &str) -> bool {
    value
        .split(|character: char| character.is_whitespace() || matches!(character, ',' | ';'))
        .map(|token| token.trim_matches(['`', '\'', '"', '(', ')']))
        .filter(|token| !token.is_empty())
        .any(|token| repo_root.join(token).exists())
}

fn ensure_deferred(feature: &Feature) -> Result<(), Error> {
    if feature.field("owner").trim().is_empty() {
        return Err(Error::Invalid(format!(
            "DEFERRED feature `{}` has no owner",
            feature.field("name")
        )));
    }
    for key in ["reason", "target_milestone"] {
        if feature
            .deferral
            .get(key)
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            return Err(Error::Invalid(format!(
                "DEFERRED feature `{}` has no deferral `{key}`",
                feature.field("name")
            )));
        }
    }
    Ok(())
}

fn check_file_sizes(repo_root: &Path, inventory: &Inventory) -> Result<(), Error> {
    let large_files = sdk_source_files(repo_root)?;
    let tracked = inventory
        .features
        .iter()
        .filter_map(|feature| {
            feature
                .fields
                .get("file_size_path")
                .map(|path| (path.clone(), feature))
        })
        .collect::<HashMap<_, _>>();

    for (relative_path, lines) in large_files {
        if lines <= MAX_SDK_FILE_LINES {
            continue;
        }
        let Some(feature) = tracked.get(&relative_path) else {
            return Err(Error::Invalid(format!(
                "{relative_path} has {lines} lines and is missing from {INVENTORY_PATH}"
            )));
        };
        if feature.field("notes").trim().is_empty() {
            return Err(Error::Invalid(format!(
                "{relative_path} has {lines} lines but its inventory row has no notes"
            )));
        }
    }

    Ok(())
}

fn sdk_source_files(repo_root: &Path) -> Result<Vec<(String, usize)>, Error> {
    let root = repo_root.join(SDK_SRC_ROOT);
    let mut files = Vec::new();
    collect_rs_files(repo_root, &root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn collect_rs_files(
    repo_root: &Path,
    directory: &Path,
    files: &mut Vec<(String, usize)>,
) -> Result<(), Error> {
    for entry in fs::read_dir(directory).map_err(|source| Error::Io {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| Error::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(repo_root, &path, files)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        let relative = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push((relative, text.lines().count()));
    }
    Ok(())
}

fn render_markdown(inventory: &Inventory) -> String {
    let mut output = String::from("| Feature | Result | Linux | macOS | Windows | Notes |\n");
    output.push_str("|---|---:|---:|---:|---:|---|\n");
    for feature in &inventory.features {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            escape_markdown(feature.field("name")),
            feature.field("result"),
            feature.field("linux"),
            feature.field("macos"),
            feature.field("windows"),
            escape_markdown(feature.field("notes")),
        ));
    }
    output
}

fn escape_markdown(value: &str) -> String {
    value.replace('|', r"\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::{check_inventory, parse_inventory};
    use std::path::Path;

    #[test]
    fn parses_verified_feature_row() {
        let text = r#"
version: 1
features:
  - name: "Quickstart"
    api_item: "rmux_sdk::Rmux::connect_or_start"
    crate_module: "rmux-sdk::handles::rmux"
    unit_test: "none"
    integration_test: "none"
    smoke_acceptance: "none"
    example_rustdoc: "Cargo.toml"
    linux: "pass"
    macos: "skipped"
    windows: "skipped"
    result: "VERIFIED"
    owner: ""
    notes: "covered"
"#;
        let inventory = parse_inventory(text).expect("inventory parses");
        check_inventory(
            Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap(),
            &inventory,
        )
        .expect("inventory checks");
    }

    #[test]
    fn deferred_feature_requires_owner_and_reason() {
        let text = r#"
version: 1
features:
  - name: "Deferred"
    api_item: "x"
    crate_module: "x"
    unit_test: "none"
    integration_test: "none"
    smoke_acceptance: "none"
    example_rustdoc: "none"
    linux: "deferred"
    macos: "deferred"
    windows: "deferred"
    result: "DEFERRED"
    owner: ""
    notes: "missing owner"
"#;
        let inventory = parse_inventory(text).expect("inventory parses");
        let error = check_inventory(Path::new("."), &inventory).expect_err("owner is required");
        assert!(error.to_string().contains("has no owner"));
    }
}
