//! The release qualification gate must run on the release host's interpreter.
//!
//! macOS ships CPython 3.9 as `/usr/bin/python3`, and `tomllib` only exists
//! from 3.11 on. `scripts/release-review-gate.sh` runs under `set -euo
//! pipefail`, so a release script that imports `tomllib` aborts the gate at its
//! changelog step: the divergence ledger, feature inventory, worktree hygiene,
//! platform neutrality and every later section never run, and the release
//! machine cannot produce gate evidence at all.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rmux-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).expect("create temp directory");
    path
}

fn describe(script: &str, output: &Output) -> String {
    format!(
        "{script} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// Every release-tooling Python invocation the gate makes, with arguments that
/// reach the manifest/ledger parse rather than stopping in `argparse`.
fn release_python_steps(output_dir: &Path) -> Vec<(&'static str, Vec<String>)> {
    let intent = output_dir.join("candidate-intent.json");
    let sha = "0".repeat(40);
    vec![
        ("scripts/toml_reader.py", vec!["--self-test".to_owned()]),
        (
            "scripts/check-changelog-release.py",
            vec!["CHANGELOG.md".to_owned()],
        ),
        ("scripts/check-tmux-release-ledger.py", Vec::new()),
        (
            "scripts/release/validate-candidate-intent.py",
            vec![
                "--expected-source-sha".to_owned(),
                sha.clone(),
                "--actual-source-sha".to_owned(),
                sha,
                "--fast-run-id".to_owned(),
                "1".to_owned(),
                "--release-intent-id".to_owned(),
                "rmux-release-gate-python-floor-spec".to_owned(),
                "--planned-release-ref".to_owned(),
                format!("v{}", env!("CARGO_PKG_VERSION")),
                "--release-kind".to_owned(),
                "stable".to_owned(),
                "--github-ref".to_owned(),
                "refs/heads/main".to_owned(),
                "--github-run-attempt".to_owned(),
                "1".to_owned(),
                "--output".to_owned(),
                intent.to_string_lossy().into_owned(),
            ],
        ),
    ]
}

fn run_with(
    interpreter: &Path,
    script: &str,
    args: &[String],
    pythonpath: Option<&Path>,
) -> Output {
    let mut command = Command::new(interpreter);
    command.arg(script).args(args).current_dir(repo_root());
    if let Some(path) = pythonpath {
        command.env("PYTHONPATH", path);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("failed to run {script}: {error}"))
}

/// Reproduce the release host on any interpreter: a `PYTHONPATH` entry that
/// hides `tomllib` even from a 3.11+ CPython, so the gate is proven portable
/// instead of proven only where the newer stdlib happens to exist.
fn interpreter_without_tomllib() -> PathBuf {
    let shim = temp_dir("tomllib-hidden");
    fs::write(
        shim.join("sitecustomize.py"),
        "import sys\n\nsys.modules[\"tomllib\"] = None\n",
    )
    .expect("write sitecustomize shim");
    fs::write(
        shim.join("tomllib.py"),
        "raise ImportError(\"tomllib is absent from the macOS release interpreter\")\n",
    )
    .expect("write tomllib shim");
    shim
}

#[test]
#[cfg(unix)]
fn release_gate_python_steps_run_without_tomllib() {
    let shim = interpreter_without_tomllib();
    let interpreter = PathBuf::from("python3");

    let probe = run_with(
        &interpreter,
        "-c",
        &["import tomllib".to_owned()],
        Some(&shim),
    );
    assert!(
        !probe.status.success(),
        "the spec cannot hide tomllib from this interpreter, so it proves nothing"
    );

    for (script, args) in release_python_steps(&shim) {
        let output = run_with(&interpreter, script, &args, Some(&shim));
        assert!(output.status.success(), "{}", describe(script, &output));
    }

    fs::remove_dir_all(&shim).expect("remove tomllib shim directory");
}

#[test]
#[cfg(target_os = "macos")]
fn release_gate_python_steps_run_with_macos_system_python() {
    let interpreter = PathBuf::from("/usr/bin/python3");
    if !interpreter.exists() {
        panic!("the macOS release host must expose /usr/bin/python3");
    }
    let outputs = temp_dir("release-gate-system-python");

    for (script, args) in release_python_steps(&outputs) {
        let output = run_with(&interpreter, script, &args, None);
        assert!(output.status.success(), "{}", describe(script, &output));
    }

    fs::remove_dir_all(&outputs).expect("remove system-python output directory");
}

fn collect_python_sources(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read release tooling directory") {
        let path = entry.expect("release tooling directory entry").path();
        if path.is_dir() {
            collect_python_sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "py") {
            found.push(path);
        }
    }
}

/// Usage patterns, not bare names: `scripts/toml_reader.py` documents the
/// stdlib module it exists to replace, and the rewritten pairing call sites name
/// the keyword they can no longer pass.
///
/// The `strict=` rule is deliberately keyword-scoped rather than
/// `zip(`-scoped. A previous pass matched `zip(` and `strict=` on one line and
/// missed `scripts/release/candidate-manifest.py`, where the keyword sat on the
/// continuation line of a wrapped call. `Path.resolve(strict=)` predates the
/// floor and is the only allowed pairing keyword; any other one must be
/// reviewed against Python 3.9 before it is added here.
fn post_floor_usage(line: &str) -> Option<&'static str> {
    const TOMLLIB: [&str; 5] = [
        "import tomllib",
        "from tomllib",
        "tomllib.",
        "\"tomllib\"",
        "'tomllib'",
    ];
    if TOMLLIB.iter().any(|needle| line.contains(needle)) {
        return Some("tomllib needs Python 3.11");
    }
    if line.contains("strict=") && !line.contains(".resolve(strict=") {
        return Some("the strict= pairing keyword needs Python 3.10");
    }
    None
}

#[test]
fn release_tooling_stays_within_the_macos_python_floor() {
    let root = repo_root();
    let mut sources = Vec::new();
    collect_python_sources(&root.join("scripts"), &mut sources);
    for workflow in fs::read_dir(root.join(".github/workflows")).expect("read workflows") {
        sources.push(workflow.expect("workflow entry").path());
    }
    sources.sort();
    assert!(
        sources.len() > 20,
        "release tooling inventory collapsed to {} files",
        sources.len()
    );

    let mut offenders = Vec::new();
    for source in sources {
        let text = fs::read_to_string(&source).expect("read release tooling source");
        for (number, line) in text.lines().enumerate() {
            if let Some(reason) = post_floor_usage(line) {
                let relative = source.strip_prefix(&root).unwrap_or(&source);
                offenders.push(format!("{}:{}: {reason}", relative.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "release tooling cannot run on the macOS release host's Python 3.9: {offenders:#?}"
    );
}

#[test]
fn both_release_gates_self_test_the_toml_reader_before_reading_manifests() {
    let unix = include_str!("../scripts/release-review-gate.sh");
    let windows = include_str!("../scripts/release-review-gate-windows.ps1");

    let unix_self_test = unix
        .find("python3 scripts/toml_reader.py --self-test")
        .expect("Unix gate lost the TOML reader self-test");
    let unix_changelog = unix
        .find("python3 scripts/check-changelog-release.py")
        .expect("Unix gate lost the changelog audit");
    assert!(
        unix_self_test < unix_changelog,
        "the Unix gate must prove the TOML reader before a manifest step depends on it"
    );

    let windows_self_test = windows
        .find("Run-PythonScript \"scripts\\toml_reader.py\" @(\"--self-test\")")
        .expect("Windows gate lost the TOML reader self-test");
    let windows_changelog = windows
        .find("Run-PythonScript \"scripts\\check-changelog-release.py\"")
        .expect("Windows gate lost the changelog audit");
    assert!(
        windows_self_test < windows_changelog,
        "the Windows gate must prove the TOML reader before a manifest step depends on it"
    );
}
