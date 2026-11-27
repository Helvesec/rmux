//! The small subset of Cargo metadata format 1 needed by this contract.
//!
//! Unknown Cargo fields are deliberately ignored; canonical artifacts use a
//! strict schema instead. Cargo owns TOML parsing, inheritance and discovery.

use super::{execute, required_nullable, sorted, Error, Package, Target};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize)]
pub(super) struct Metadata {
    version: u32,
    pub(super) workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    manifest_path: PathBuf,
    name: String,
    version: String,
    edition: String,
    #[serde(deserialize_with = "required_nullable")]
    rust_version: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    publish: Option<Vec<String>>,
    targets: Vec<CargoTarget>,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
    src_path: PathBuf,
    kind: Vec<String>,
    crate_types: Vec<String>,
    #[serde(rename = "required-features", default)]
    required_features: Vec<String>,
}

pub(super) fn read(directory: &Path) -> Result<Metadata, Error> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = execute(Command::new(cargo).current_dir(directory).args([
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--locked",
        "--offline",
    ]))?;
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub(super) struct Projection {
    pub(super) packages: Vec<Package>,
    pub(super) inputs: Vec<PathBuf>,
}

pub(super) fn packages(metadata: Metadata) -> Result<Projection, Error> {
    if metadata.version != 1 || metadata.workspace_members.is_empty() {
        return Err(Error::Invalid(
            "expected Cargo metadata v1 with workspace members".into(),
        ));
    }
    let mut members: BTreeSet<_> = metadata.workspace_members.iter().cloned().collect();
    if members.len() != metadata.workspace_members.len() {
        return Err(Error::Invalid("duplicate Cargo workspace member".into()));
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut packages = Vec::new();
    let mut inputs = vec![metadata.workspace_root.join("Cargo.toml")];
    for package in metadata.packages {
        if !ids.insert(package.id.clone()) {
            return Err(Error::Invalid("duplicate Cargo package ID".into()));
        }
        if !members.remove(&package.id) {
            continue;
        }
        if !names.insert(package.name.clone()) {
            return Err(Error::Invalid(
                "workspace package names must be unique".into(),
            ));
        }
        let id = format!("package:{}", package.name);
        inputs.push(package.manifest_path);
        let mut targets = Vec::new();
        for target in package.targets {
            let input = target.src_path.clone();
            if let Some(target) = target_surface(&id, target)? {
                inputs.push(input);
                targets.push(target);
            }
        }
        targets.sort_by(|a, b| a.id.cmp(&b.id));
        if targets.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(Error::Invalid(format!("duplicate target ID in {id}")));
        }
        let mut publish = package.publish;
        if let Some(registries) = &mut publish {
            sorted(registries);
        }
        let mut features = package.features;
        for members in features.values_mut() {
            sorted(members);
        }
        packages.push(Package {
            id,
            name: package.name,
            version: package.version,
            edition: package.edition,
            rust_version: package.rust_version,
            publish,
            targets,
            features,
        });
    }
    if !members.is_empty() {
        return Err(Error::Invalid(
            "Cargo metadata omitted a workspace member".into(),
        ));
    }
    packages.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Projection { packages, inputs })
}

fn target_surface(package_id: &str, mut target: CargoTarget) -> Result<Option<Target>, Error> {
    let mut surface = None;
    for kind in &target.kind {
        match kind.as_str() {
            "bin" => surface = Some("bin"),
            "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => {
                surface = Some("lib")
            }
            "example" | "test" | "bench" | "custom-build" => return Ok(None),
            _ => {
                return Err(Error::Invalid(format!(
                    "unrecognized Cargo target kind: {kind}"
                )))
            }
        }
    }
    let surface = surface.ok_or_else(|| Error::Invalid("empty Cargo target kind".into()))?;
    sorted(&mut target.kind);
    sorted(&mut target.crate_types);
    sorted(&mut target.required_features);
    Ok(Some(Target {
        id: format!("{package_id}/{surface}:{}", target.name),
        name: target.name,
        kind: target.kind,
        crate_types: target.crate_types,
        required_features: target.required_features,
    }))
}
