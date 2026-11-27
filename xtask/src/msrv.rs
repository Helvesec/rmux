//! Exact-compiler, locked source qualification; not a release or installed gate.

use crate::package_inventory::Error;
use serde_json::{json, Value};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

mod cells;
mod context;
#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod tests;

const FLOOR: &str = env!("CARGO_PKG_RUST_VERSION");

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Options {
    toolchain: PathBuf,
    output: PathBuf,
    target: PathBuf,
}

impl Options {
    pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = args;
        let (mut toolchain, mut output, mut target) = (None, None, None);
        while let Some(flag) = args.next() {
            let slot = match flag.as_str() {
                "--toolchain-dir" => &mut toolchain,
                "--output-dir" => &mut output,
                "--target-dir" => &mut target,
                _ => return Err(format!("unknown msrv argument: {flag}")),
            };
            let path = PathBuf::from(args.next().ok_or_else(|| format!("missing {flag} value"))?);
            if !path.is_absolute() || slot.replace(path).is_some() {
                return Err(format!("{flag} requires one absolute path"));
            }
        }
        Ok(Self {
            toolchain: toolchain.ok_or("msrv requires --toolchain-dir")?,
            output: output.ok_or("msrv requires --output-dir")?,
            target: target.ok_or("msrv requires --target-dir")?,
        })
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn output(command: &mut Command) -> Result<String, Error> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(invalid(format!(
            "{command:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    String::from_utf8(result.stdout).map_err(|_| invalid("non-UTF-8 command output"))
}

fn git(root: &Path, args: &[&str]) -> Result<String, Error> {
    output(Command::new("git").current_dir(root).args(args))
}

fn clean(root: &Path) -> Result<(), Error> {
    if !git(root, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err(invalid("msrv requires a clean committed workspace"));
    }
    Ok(())
}

fn exact_version(name: &str, version: &str) -> Result<(), Error> {
    if version.split_whitespace().take(2).collect::<Vec<_>>() != [name, FLOOR] {
        return Err(invalid(format!("expected {name} {FLOOR}, got {version}")));
    }
    Ok(())
}

/// A guard against two instances of this gate sharing mutable Cargo outputs.
struct TargetLock(PathBuf);

impl Drop for TargetLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Runner {
    root: PathBuf,
    options: Options,
    cargo: PathBuf,
    cargo_home: Option<std::ffi::OsString>,
    rustc: PathBuf,
    rustdoc: PathBuf,
    path: std::ffi::OsString,
    host: String,
    receipts: File,
    count: usize,
}

fn cargo_output(command: &mut Command) -> Result<String, Error> {
    context::check(command)?;
    output(command)
}

impl Runner {
    fn metadata(&self) -> Command {
        let mut command = Command::new(&self.cargo);
        if let Some(home) = &self.cargo_home {
            command.env("CARGO_HOME", home);
        }
        command
            .current_dir(&self.root)
            .env("RUSTC", &self.rustc)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--offline",
                "--all-features",
            ]);
        command
    }

    fn cargo(&self, directory: &Path, args: &[String], target: &str) -> Command {
        let mut command = Command::new(&self.cargo);
        if let Some(home) = &self.cargo_home {
            command.env("CARGO_HOME", home);
        }
        command
            .current_dir(directory)
            .args(args)
            .args([
                "--locked",
                "--offline",
                "--jobs",
                "2",
                "--target",
                target,
                "--target-dir",
            ])
            .arg(&self.options.target)
            .env("CARGO", &self.cargo)
            .env("RUSTC", &self.rustc)
            .env("RUSTDOC", &self.rustdoc)
            .env("PATH", &self.path);
        command
    }

    fn cell(
        &mut self,
        id: &str,
        command: &mut Command,
        rejection: Option<&str>,
    ) -> Result<(), Error> {
        context::check(command)?;
        let log_path = self.options.output.join(format!("{id}.log"));
        let mut log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log_path)?;
        writeln!(log, "{command:?}")?;
        let start = Instant::now();
        println!("[msrv] {id}");
        let status = command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()?;
        let accepted = match rejection {
            None => status.success(),
            Some(diagnostic) => {
                status.code() == Some(101) && fs::read_to_string(&log_path)?.contains(diagnostic)
            }
        };
        writeln!(
            self.receipts,
            "{}",
            json!({
                "cell": id, "command": format!("{command:?}"), "exit_code": status.code(),
                "expected_rejection": rejection, "accepted": accepted,
                "seconds": start.elapsed().as_secs_f64(), "log": log_path,
            })
        )?;
        self.receipts.flush()?;
        self.count += 1;
        if !accepted {
            return Err(invalid(format!(
                "cell {id} failed; see {}",
                log_path.display()
            )));
        }
        Ok(())
    }

    fn witness(&mut self) -> Result<(), Error> {
        // Copy only committed inputs into a separate workspace; never mutate the candidate.
        let source = self.options.output.join("higher-requirement-source");
        fs::create_dir(&source)?;
        for file in git(&self.root, &["ls-files", "-z"])?
            .split('\0')
            .filter(|s| !s.is_empty())
        {
            let from = self.root.join(file);
            if !fs::symlink_metadata(&from)?.is_file() {
                return Err(invalid(format!("unsupported witness input: {file}")));
            }
            let to = source.join(file);
            fs::create_dir_all(to.parent().ok_or_else(|| invalid("missing parent"))?)?;
            fs::copy(from, to)?;
        }
        let args = cells::words(&[
            "check",
            "-p",
            "rmux-sdk",
            "--lib",
            "--no-default-features",
            "--features",
            "regex",
        ]);
        self.cell(
            "witness-control",
            &mut self.cargo(&source, &args, &self.host),
            None,
        )?;
        let manifest = source.join("crates/rmux-sdk/Cargo.toml");
        let original = fs::read_to_string(&manifest)?;
        let mut version = FLOOR.split('.');
        let major = version
            .next()
            .ok_or_else(|| invalid("missing MSRV major"))?;
        let minor: u32 = version
            .next()
            .ok_or_else(|| invalid("missing MSRV minor"))?
            .parse()
            .map_err(|_| invalid("invalid MSRV minor"))?;
        let higher = format!("{major}.{}.0", minor + 1);
        let needle = "rust-version.workspace = true";
        if original.matches(needle).count() != 1 {
            return Err(invalid(
                "witness requires exactly one SDK MSRV inheritance declaration",
            ));
        }
        fs::write(
            &manifest,
            original.replace(needle, &format!("rust-version = {higher:?}")),
        )?;
        let metadata: Value = serde_json::from_str(&fs::read_to_string(
            self.options.output.join("metadata.json"),
        )?)?;
        let sdk = metadata["packages"]
            .as_array()
            .ok_or_else(|| invalid("missing packages"))?
            .iter()
            .find(|p| p["name"] == "rmux-sdk")
            .ok_or_else(|| invalid("missing SDK"))?;
        let version = sdk["version"]
            .as_str()
            .ok_or_else(|| invalid("missing SDK version"))?;
        let diagnostic = format!("rmux-sdk@{version} requires rustc {higher}");
        self.cell(
            "witness-higher-requirement",
            &mut self.cargo(&source, &args, &self.host),
            Some(&diagnostic),
        )?;
        // Retain the mutation and raw failure, then prove that restoring only it recovers.
        fs::copy(
            &manifest,
            self.options.output.join("higher-requirement-Cargo.toml"),
        )?;
        fs::write(&manifest, original)?;
        self.cell(
            "witness-restored",
            &mut self.cargo(&source, &args, &self.host),
            None,
        )?;
        if fs::read(source.join("Cargo.lock"))? != fs::read(self.root.join("Cargo.lock"))? {
            return Err(invalid("witness changed the locked dependency graph"));
        }
        Ok(())
    }
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    let root = fs::canonicalize(env::current_dir()?)?;
    if fs::canonicalize(git(&root, &["rev-parse", "--show-toplevel"])?.trim())? != root {
        return Err(invalid("run msrv at the workspace Git root"));
    }
    clean(&root)?;
    fs::create_dir(&options.output)?;
    fs::create_dir_all(&options.target)?;
    let lock = options.target.join(".rmux-msrv-lock");
    writeln!(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)?,
        "{}",
        std::process::id()
    )?;
    let _lock = TargetLock(lock);
    let binary = |name: &str| {
        options
            .toolchain
            .join("bin")
            .join(format!("{name}{}", env::consts::EXE_SUFFIX))
    };
    let (cargo, rustc, rustdoc) = (binary("cargo"), binary("rustc"), binary("rustdoc"));
    let rust_version = output(Command::new(&rustc).arg("-vV"))?;
    let cargo_version = output(Command::new(&cargo).arg("-V"))?;
    let doc_version = output(Command::new(&rustdoc).arg("-V"))?;
    fs::write(
        options.output.join("tools.txt"),
        format!(
            "{rustc:?}\n{rust_version}\n{cargo:?}\n{cargo_version}\n{rustdoc:?}\n{doc_version}"
        ),
    )?;
    exact_version("rustc", &rust_version)?;
    exact_version("cargo", &cargo_version)?;
    exact_version("rustdoc", &doc_version)?;
    let host = rust_version
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or_else(|| invalid("rustc omitted native host"))?
        .to_owned();
    let mut paths = vec![options.toolchain.join("bin")];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    let path = env::join_paths(paths).map_err(|_| invalid("invalid toolchain PATH"))?;
    let receipts = File::create(options.output.join("receipts.jsonl"))?;
    let mut runner = Runner {
        root,
        options,
        cargo,
        cargo_home: env::var_os("CARGO_HOME"),
        rustc,
        rustdoc,
        path,
        host,
        receipts,
        count: 0,
    };
    let lock_before = fs::read(runner.root.join("Cargo.lock"))?;
    let environment: std::collections::BTreeMap<_, _> = env::vars()
        .filter(|(key, _)| {
            // Compiler configuration only: never record registry credentials or account settings.
            key == "RUSTFLAGS"
                || key == "CARGO_ENCODED_RUSTFLAGS"
                || key == "RUSTDOCFLAGS"
                || key == "CARGO_HOME"
                || key == "RUST_MIN_STACK"
                || key == "RUST_TEST_THREADS"
                || (key.starts_with("CARGO_TARGET_")
                    && (key.ends_with("_RUSTFLAGS") || key.ends_with("_LINKER")))
        })
        .collect();
    fs::write(
        runner.options.output.join("source.json"),
        serde_json::to_vec_pretty(&json!({
            "commit": git(&runner.root, &["rev-parse", "HEAD"])?.trim(),
            "tree": git(&runner.root, &["rev-parse", "HEAD^{tree}"])?.trim(),
            "floor": FLOOR, "host": runner.host, "environment": environment,
        }))?,
    )?;
    let metadata = cargo_output(&mut runner.metadata())?;
    fs::write(runner.options.output.join("metadata.json"), &metadata)?;
    let metadata: Value = serde_json::from_str(&metadata)?;
    let plan = cells::plan(&metadata, &runner.host)?;
    fs::write(
        runner.options.output.join("plan.json"),
        serde_json::to_vec_pretty(&plan)?,
    )?;
    for cell in &plan {
        runner.cell(
            &cell.id,
            &mut runner.cargo(&runner.root, &cell.args, &cell.target),
            None,
        )?;
    }
    runner.witness()?;
    if lock_before != fs::read(runner.root.join("Cargo.lock"))? {
        return Err(invalid("MSRV check changed Cargo.lock"));
    }
    clean(&runner.root)?;
    println!(
        "[msrv] PASS: {} cells on {}; exact Rust {FLOOR}; source compilation only",
        runner.count, runner.host
    );
    Ok(())
}
