//! Cargo's effective workspace package, feature and library/binary declarations.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod metadata;
mod provenance;
#[cfg(test)]
mod tests;

const BASELINE_PATH: &str = "xtask/spec/package-inventory-v1.json";
const SCOPE: &str = "workspace-package-feature-library-binary-declarations";

/// Explicit inventory operation; checking never updates the expected artifact.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Compare live Cargo declarations with the canonical baseline.
    Check,
    /// Print an inventory from a clean, committed workspace to stdout.
    Export,
    /// Replace the baseline from a clean, committed workspace.
    Update,
}

/// Inventory input, process or contract failure.
#[derive(Debug)]
pub(crate) enum Error {
    /// A local filesystem or process operation failed.
    Io(std::io::Error),
    /// Cargo metadata or the baseline is not valid JSON of the required shape.
    Json(serde_json::Error),
    /// An executable failed or a contract invariant was violated.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Inventory {
    schema_version: u32,
    scope: String,
    metadata_format: u32,
    source_base: SourceBase,
    packages: Vec<Package>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceBase {
    commit: String,
    tree: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Package {
    id: String,
    name: String,
    version: String,
    edition: String,
    // Custom deserialization makes absence invalid while preserving explicit null.
    #[serde(deserialize_with = "required_nullable")]
    rust_version: Option<String>,
    // Cargo: null = unrestricted, [] = disabled, nonempty = allowed registries.
    #[serde(deserialize_with = "required_nullable")]
    publish: Option<Vec<String>>,
    targets: Vec<Target>,
    // Includes Cargo's implicit optional-dependency features. The default key is
    // retained even when its value is empty; absence is a different declaration.
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Target {
    id: String,
    name: String,
    kind: Vec<String>,
    crate_types: Vec<String>,
    required_features: Vec<String>,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

fn sorted(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn canonical(inventory: &Inventory) -> Result<String, Error> {
    Ok(serde_json::to_string_pretty(inventory)? + "\n")
}

fn inventory(packages: Vec<Package>, source_base: SourceBase) -> Inventory {
    Inventory {
        schema_version: 1,
        scope: SCOPE.to_owned(),
        metadata_format: 1,
        source_base,
        packages,
    }
}

fn check(expected: &str, packages: Vec<Package>) -> Result<(), Error> {
    let baseline: Inventory = serde_json::from_str(expected)?;
    if baseline.schema_version != 1 || baseline.scope != SCOPE || baseline.metadata_format != 1 {
        return Err(Error::Invalid(
            "unsupported package inventory schema or scope".into(),
        ));
    }
    for hash in [&baseline.source_base.commit, &baseline.source_base.tree] {
        if hash.len() != 40
            || !hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::Invalid(
                "source_base requires full lowercase Git SHA-1 IDs".into(),
            ));
        }
    }
    let current = inventory(packages, baseline.source_base);
    if baseline.packages != current.packages {
        let before: BTreeMap<_, _> = baseline.packages.iter().map(|p| (&p.id, p)).collect();
        let after: BTreeMap<_, _> = current.packages.iter().map(|p| (&p.id, p)).collect();
        let ids: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
        let changes: Vec<_> = ids
            .into_iter()
            .filter_map(|id| match (before.get(id), after.get(id)) {
                (None, Some(_)) => Some(format!("added {id}")),
                (Some(_), None) => Some(format!("removed {id}")),
                (Some(a), Some(b)) if a != b => Some(format!("changed {id}")),
                _ => None,
            })
            .collect();
        return Err(Error::Invalid(format!(
            "package inventory mismatch (including ordering/duplicates): {}; review declarations before --update",
            changes.join(", ")
        )));
    }
    // Exact bytes also reject duplicate JSON map keys, unknown representation,
    // reordered keys, extra paths, missing fields and noncanonical whitespace.
    if expected != canonical(&current)? {
        return Err(Error::Invalid(
            "package baseline is not canonical; review before --update".into(),
        ));
    }
    Ok(())
}

fn execute(command: &mut Command) -> Result<Output, Error> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "{} failed ({}): {}",
            command.get_program().to_string_lossy(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}

fn git(root: &Path, args: &[&str]) -> Result<String, Error> {
    let output = execute(Command::new("git").current_dir(root).args(args))?;
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|_| Error::Invalid("non-UTF-8 Git output".into()))
}

fn source_base(root: &Path) -> Result<SourceBase, Error> {
    let git_root = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if fs::canonicalize(git_root)? != fs::canonicalize(root)? {
        return Err(Error::Invalid(
            "export/update requires the workspace at the Git root".into(),
        ));
    }
    if !git(root, &["status", "--porcelain", "--untracked-files=normal"])?.is_empty() {
        return Err(Error::Invalid(
            "export/update requires a clean committed workspace; commit source changes first"
                .into(),
        ));
    }
    Ok(SourceBase {
        commit: git(root, &["rev-parse", "HEAD"])?,
        tree: git(root, &["rev-parse", "HEAD^{tree}"])?,
    })
}

/// Operate on the workspace containing `directory`, using Cargo rather than
/// the xtask binary's build-time source path. Export/update records its clean
/// source commit; check retains that historical provenance, never current HEAD.
pub(crate) fn run(mode: Mode, directory: &Path) -> Result<Option<String>, Error> {
    let metadata = metadata::read(directory)?;
    let root = metadata.workspace_root.clone();
    let metadata::Projection { packages, inputs } = metadata::packages(metadata)?;
    let path = root.join(BASELINE_PATH);
    match mode {
        Mode::Check => {
            check(&fs::read_to_string(path)?, packages)?;
            Ok(None)
        }
        Mode::Export | Mode::Update => {
            let source = source_base(&root)?;
            provenance::verify(&root, &source.tree, &inputs)?;
            let output = canonical(&inventory(packages, source))?;
            if mode == Mode::Export {
                Ok(Some(output))
            } else {
                fs::write(path, output)?;
                Ok(None)
            }
        }
    }
}
