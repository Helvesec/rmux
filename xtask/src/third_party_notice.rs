//! Third-party notices for the packages observed in completed Cargo builds.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

mod supplements;

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("missing string field: {key}"))
}

fn json(input: &str) -> Result<Value, String> {
    serde_json::from_str(input).map_err(|error| error.to_string())
}

/// Only compiler-artifact events select packages. Metadata supplies descriptions,
/// never the population: optional/unselected lockfile entries must stay absent.
fn observed(logs: &[PathBuf]) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    if logs.is_empty() {
        return Err("at least one completed Cargo build log is required".into());
    }
    let mut packages = BTreeMap::<String, BTreeSet<String>>::new();
    for path in logs {
        let input = read(path)?;
        if !input.ends_with('\n') {
            return Err(format!("truncated Cargo JSONL: {}", path.display()));
        }
        let mut finished = false;
        let mut roots = BTreeSet::new();
        for line in input.lines() {
            if finished {
                return Err("event after terminal build-finished".into());
            }
            let event = json(line)?;
            match text(&event, "reason")? {
                "compiler-artifact" => {
                    if event["profile"]["test"].as_bool() != Some(false) {
                        return Err("test artifacts cannot describe a production build".into());
                    }
                    let package = text(&event, "package_id")?.to_owned();
                    let target = &event["target"];
                    let name = text(target, "name")?;
                    let kinds = target["kind"].as_array().ok_or("missing target kind")?;
                    if kinds.is_empty() || !kinds.iter().all(Value::is_string) {
                        return Err("invalid target kinds".into());
                    }
                    if !event["executable"].is_null()
                        && (kinds != &[Value::String("bin".into())]
                            || !roots.insert((package.clone(), name.to_owned())))
                    {
                        return Err("ambiguous or non-binary payload root".into());
                    }
                    let features = event["features"].as_array().ok_or("missing features")?;
                    if !features.iter().all(Value::is_string) {
                        return Err("invalid features".into());
                    }
                    packages.entry(package).or_default().insert(format!(
                        "{name}: kinds={}, features={}",
                        target["kind"], event["features"]
                    ));
                }
                "compiler-message" | "build-script-executed" => {}
                "build-finished" if event["success"].as_bool() == Some(true) => finished = true,
                _ => return Err("not a successful Cargo build stream".into()),
            }
        }
        if !finished || roots.is_empty() {
            return Err("missing successful build-finished or binary payload root".into());
        }
    }
    Ok(packages)
}

/// SPDX AND binds more tightly than OR. WITH is an indivisible exception
/// choice and must itself appear on the policy allowlist. Parse both branches
/// even when an OR branch already satisfies the policy.
struct Expression<'a> {
    tokens: Vec<String>,
    cursor: usize,
    allow: &'a BTreeSet<String>,
}

impl Expression<'_> {
    fn take(&mut self, token: &str) -> bool {
        if self
            .tokens
            .get(self.cursor)
            .is_some_and(|next| next == token)
        {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn atom(&mut self) -> Result<Option<BTreeSet<String>>, String> {
        if self.take("(") {
            let value = self.or()?;
            if !self.take(")") {
                return Err("unclosed license expression".into());
            }
            return Ok(value);
        }
        let mut id = self
            .tokens
            .get(self.cursor)
            .ok_or("missing license identifier")?
            .clone();
        if matches!(id.as_str(), "AND" | "OR" | "WITH" | ")")
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+'))
        {
            return Err("invalid license identifier".into());
        }
        self.cursor += 1;
        if self.take("WITH") {
            let exception = self
                .tokens
                .get(self.cursor)
                .ok_or("missing license exception")?;
            if !exception
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
                || matches!(exception.as_str(), "AND" | "OR" | "WITH")
            {
                return Err("invalid license exception".into());
            }
            id.push_str(" WITH ");
            id.push_str(exception);
            self.cursor += 1;
        }
        Ok(self.allow.contains(&id).then(|| BTreeSet::from([id])))
    }

    fn and(&mut self) -> Result<Option<BTreeSet<String>>, String> {
        let mut value = self.atom()?;
        while self.take("AND") {
            let next = self.atom()?;
            value = match (value, next) {
                (Some(mut left), Some(right)) => {
                    left.extend(right);
                    Some(left)
                }
                _ => None,
            };
        }
        Ok(value)
    }

    fn or(&mut self) -> Result<Option<BTreeSet<String>>, String> {
        let mut value = self.and()?;
        while self.take("OR") {
            let next = self.and()?;
            value = value.or(next);
        }
        Ok(value)
    }
}

fn allowed(expression: &str, allow: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
    let normalized = expression
        .replace('/', " OR ")
        .replace('(', " ( ")
        .replace(')', " ) ");
    let mut parser = Expression {
        tokens: normalized.split_whitespace().map(str::to_owned).collect(),
        cursor: 0,
        allow,
    };
    let choice = parser.or()?;
    if parser.cursor != parser.tokens.len() {
        return Err(format!("invalid license expression: {expression}"));
    }
    choice.ok_or_else(|| format!("license outside deny.toml allowlist: {expression}"))
}

fn license_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "COPYRIGHT",
        "NOTICE",
        "UNLICENSE",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn license_files(directory: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut files = BTreeSet::new();
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if license_name(&entry.file_name().to_string_lossy()) && entry.path().is_file() {
            files.insert(entry.path());
        }
    }
    Ok(files)
}

fn upstream_texts(package: &Value, workspace: &Path) -> Result<Vec<(String, String)>, String> {
    let manifest = Path::new(text(package, "manifest_path")?);
    let directory = manifest.parent().ok_or("package manifest has no parent")?;
    let mut base = directory.to_path_buf();
    let mut files = license_files(directory)?;
    if let Some(license) = package["license_file"].as_str() {
        files.insert(directory.join(license));
    }
    // Workspace crates often inherit their license and ship its text at the
    // workspace root. Do not search parents of arbitrary registry packages.
    if files.is_empty() && package["source"].is_null() && directory.starts_with(workspace) {
        base = workspace.to_path_buf();
        files = license_files(workspace)?;
    }
    if files.is_empty() {
        if let Some(texts) = supplements::texts(package, directory)? {
            return Ok(texts);
        }
        return Err(format!(
            "missing upstream license/NOTICE text for {}",
            text(package, "name")?
        ));
    }
    let base = base.canonicalize().map_err(|error| error.to_string())?;
    files
        .into_iter()
        .map(|path| {
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if !canonical.starts_with(&base)
                || fs::symlink_metadata(&path)
                    .map_err(|e| e.to_string())?
                    .file_type()
                    .is_symlink()
            {
                return Err("license file escapes package source or is a symlink".into());
            }
            let contents = read(&path)?;
            if contents.trim().is_empty() {
                return Err(format!("empty upstream license text: {}", path.display()));
            }
            Ok((
                canonical
                    .strip_prefix(&base)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/"),
                contents,
            ))
        })
        .collect()
}

pub(crate) fn generate(
    metadata: &Path,
    build_logs: &[PathBuf],
    policy: &Path,
) -> Result<String, String> {
    let metadata = json(&read(metadata)?)?;
    let policy: toml::Value = toml::from_str(&read(policy)?).map_err(|e| e.to_string())?;
    let allow: BTreeSet<String> = policy
        .get("licenses")
        .and_then(|v| v.get("allow"))
        .and_then(toml::Value::as_array)
        .ok_or("missing licenses.allow policy")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("invalid license allowlist")
        })
        .collect::<Result<_, _>>()?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("missing Cargo packages")?;
    let mut by_id = BTreeMap::new();
    for package in packages {
        if by_id.insert(text(package, "id")?, package).is_some() {
            return Err("duplicate Cargo package identity".into());
        }
    }
    let workspace = Path::new(text(&metadata, "workspace_root")?);
    let mut selected = Vec::new();
    for (id, units) in observed(build_logs)? {
        let package = *by_id
            .get(id.as_str())
            .ok_or_else(|| format!("built package absent from metadata: {id}"))?;
        let name = text(package, "name")?;
        let version = text(package, "version")?;
        let expression = text(package, "license")?;
        let choice =
            allowed(expression, &allow).map_err(|error| format!("{name} {version}: {error}"))?;
        let files = upstream_texts(package, workspace)?;
        selected.push((
            (name.to_owned(), version.to_owned(), id),
            expression.to_owned(),
            choice,
            units,
            files,
        ));
    }
    selected.sort_by(|a, b| a.0.cmp(&b.0));
    let mut output = String::from(
        "RMUX third-party notices\n\n\
Generated from completed Cargo compiler-artifact events.\n\
Scope: observed target units and build tooling, including fresh reused units.\n\
This is not a claim of post-link reachability or an SBOM attestation.\n\
Packages resolved but not emitted by these builds are omitted.\n",
    );
    for ((name, version, _), expression, choice, units, files) in selected {
        output.push_str(&format!(
            "\n## {name} {version}\nDeclared license: {expression}\nAllowed choice: {}\n",
            choice.into_iter().collect::<Vec<_>>().join(" AND ")
        ));
        for unit in units {
            output.push_str(&format!("Observed unit: {unit}\n"));
        }
        for (file, contents) in files {
            output.push_str(&format!("\n--- {file} ---\n{contents}"));
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    Ok(output)
}

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    let (mut metadata, mut policy, mut output) = (None, None, None);
    let mut logs = Vec::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = PathBuf::from(
            args.next()
                .ok_or_else(|| format!("{flag} requires a path"))?,
        );
        let slot = match flag.as_str() {
            "--metadata" => &mut metadata,
            "--policy" => &mut policy,
            "--output" => &mut output,
            "--build-log" => {
                logs.push(value);
                continue;
            }
            _ => return Err(format!("unknown third-party-notice argument: {flag}")),
        };
        if slot.replace(value).is_some() {
            return Err(format!("duplicate argument: {flag}"));
        }
    }
    let notice = generate(
        &metadata.ok_or("--metadata is required")?,
        &logs,
        &policy.ok_or("--policy is required")?,
    )?;
    fs::write(output.ok_or("--output is required")?, notice).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod package_tests;
