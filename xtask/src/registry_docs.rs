//! One generator and byte-for-byte checker for implemented registry projections.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

mod ledger;
mod manual_context;
mod render;
mod snapshot;
#[cfg(test)]
mod tests;

/// Explicit documentation destination; checking never writes it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Options {
    output: PathBuf,
    check: bool,
}

impl Options {
    pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut output = None;
        let mut check = false;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--output-dir" if output.is_none() => {
                    let value = args
                        .next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or("registry-docs --output-dir requires a path")?;
                    output = Some(PathBuf::from(value));
                }
                "--check" if !check => check = true,
                _ => return Err(format!("unknown or repeated registry-docs argument: {arg}")),
            }
        }
        Ok(Self {
            output: output.ok_or("registry-docs requires --output-dir <path>")?,
            check,
        })
    }
}

#[derive(Debug)]
pub(crate) enum Error {
    Io { path: PathBuf, source: io::Error },
    Invalid(String),
    Stale(PathBuf),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Invalid(message) => f.write_str(message),
            Self::Stale(path) => write!(
                f,
                "stale or missing registry projection: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {}

fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_owned(),
        source,
    }
}

pub(crate) struct Artifact {
    name: &'static str,
    content: String,
    marker: &'static str,
}

/// All artifacts come from the same snapshot and the authoritative divergence ledger.
pub(crate) fn run(options: &Options) -> Result<Vec<PathBuf>, Error> {
    let snapshot = snapshot::implemented()?;
    let cwd = std::env::current_dir().map_err(|error| io_error(Path::new("."), error))?;
    let root = cwd
        .ancestors()
        .find(|path| {
            path.join("xtask/Cargo.toml").is_file()
                || path
                    .join("tests/reference/tmux_compat/divergences.toml")
                    .is_file()
        })
        .ok_or_else(|| Error::Invalid("registry-docs requires a repository source root".into()))?;
    let ledger_path = root.join("tests/reference/tmux_compat/divergences.toml");
    let text = fs::read_to_string(&ledger_path).map_err(|error| io_error(&ledger_path, error))?;
    let ledger = ledger::parse(&text)?;
    ledger.validate_references(root)?;
    let artifacts = render::artifacts(&snapshot, &ledger)?;
    preflight(&options.output, options.check, &artifacts)?;
    if options.output == Path::new("docs/generated") {
        preflight(&root.join("docs/man"), options.check, &artifacts[2..3])?;
    }
    let mut paths = publish(&options.output, options.check, &artifacts)?;
    if options.output == Path::new("docs/generated") {
        paths.extend(publish(
            &root.join("docs/man"),
            options.check,
            &artifacts[2..3],
        )?);
    }
    Ok(paths)
}

fn publish(output: &Path, check: bool, artifacts: &[Artifact]) -> Result<Vec<PathBuf>, Error> {
    let paths = preflight(output, check, artifacts)?;
    if !check {
        fs::create_dir_all(output).map_err(|error| io_error(output, error))?;
        for (artifact, path) in artifacts.iter().zip(&paths) {
            fs::write(path, &artifact.content).map_err(|error| io_error(path, error))?;
        }
    }
    Ok(paths)
}

fn preflight(output: &Path, check: bool, artifacts: &[Artifact]) -> Result<Vec<PathBuf>, Error> {
    // Preflight the whole set before generation can replace any owned output.
    // Human documents and evidence never become generator inputs by coincidence.
    let mut paths = Vec::new();
    for artifact in artifacts {
        let path = output.join(artifact.name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(Error::Invalid(format!(
                    "output is not a regular file: {}",
                    path.display()
                )));
            }
            Ok(_) => {
                let bytes = fs::read(&path).map_err(|error| io_error(&path, error))?;
                if check {
                    if bytes != artifact.content.as_bytes() {
                        return Err(Error::Stale(path));
                    }
                } else if !bytes.starts_with(artifact.marker.as_bytes()) {
                    return Err(Error::Invalid(format!(
                        "refusing to replace an unowned document: {}",
                        path.display()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if check {
                    return Err(Error::Stale(path));
                }
            }
            Err(error) => return Err(io_error(&path, error)),
        }
        paths.push(path);
    }
    Ok(paths)
}
