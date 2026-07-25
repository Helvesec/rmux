//! Guards the Tokio-runtime contract of `rmux_ipc::LocalListener::bind` and the
//! release-gate coverage of the Windows endpoint/legacy-shutdown code.

use std::fs;
use std::path::{Path, PathBuf};

const WINDOWS_RELEASE_GATE: &str = include_str!("../scripts/release-review-gate-windows.ps1");
const CI: &str = include_str!("../.github/workflows/ci.yml");

/// `LocalListener::bind` constructs Tokio I/O resources (a Unix listener, or the
/// Windows named-pipe server instances), so it panics with "there is no reactor
/// running" when it is called outside a runtime. A plain `#[test]` that binds is
/// a permanently red test that never reaches its own assertions.
#[test]
fn no_plain_test_binds_a_local_listener_without_a_runtime() {
    // This spec names the API in prose, so it must not scan itself.
    let own_source = Path::new(file!()).file_name().expect("spec file name");
    let offenders = rust_sources(&workspace_root())
        .into_iter()
        .filter(|file| file.file_name() != Some(own_source))
        .flat_map(|file| {
            let source = fs::read_to_string(&file).expect("read Rust source");
            bind_sites_without_a_runtime(&source)
                .into_iter()
                .map(|line| format!("{}:{line}", relative(&file)))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "LocalListener::bind needs a Tokio runtime; use #[tokio::test] or an \
         explicit runtime at: {}",
        offenders.join(", ")
    );
}

/// The Windows release gate is the only gate that runs during a release
/// qualification (`.github/workflows/ci.yml` skips the Windows workspace test
/// shards there), so the endpoint discovery and legacy-shutdown suites have to
/// be named steps in it.
#[test]
fn windows_release_gate_runs_the_endpoint_and_legacy_shutdown_suites() {
    for filter in [
        "\"test\", \"-p\", \"rmux-ipc\", \"--locked\", \"--test\", \"named_pipe_integration\"",
        "\"test\", \"-p\", \"rmux-client\", \"--locked\", \"--test\", \"windows_legacy_shutdown\"",
    ] {
        assert!(
            WINDOWS_RELEASE_GATE.contains(&format!("Assert-CargoFilter 1 @({filter})")),
            "Windows release gate may silently select zero tests for {filter}"
        );
        assert!(
            WINDOWS_RELEASE_GATE.contains(&format!("Run \"cargo\" @({filter},")),
            "Windows release gate never runs {filter}"
        );
    }

    // Tag builds go through `windows-runtime-smoke`, which is the only native
    // Windows CI job that survives a release-qualification dispatch.
    let smoke = ci_job("windows-runtime-smoke");
    for command in [
        "cargo test -p rmux-ipc --locked --test named_pipe_integration",
        "cargo test -p rmux-client --locked --test windows_legacy_shutdown",
    ] {
        assert!(
            smoke.contains(command),
            "native Windows runtime smoke lost {command}"
        );
    }
}

/// Returns one top-level CI job body, bounded by the next job key.
fn ci_job(name: &str) -> String {
    let header = format!("  {name}:");
    let mut lines = CI.lines();
    lines
        .by_ref()
        .find(|line| *line == header)
        .unwrap_or_else(|| panic!("missing CI job {name}"));
    lines
        .take_while(|line| !is_job_header(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_job_header(line: &str) -> bool {
    line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':')
}

/// Reports the 1-based lines of every `LocalListener::bind` call that sits in a
/// `#[test]` function with neither `#[tokio::test]` nor an explicit runtime.
fn bind_sites_without_a_runtime(source: &str) -> Vec<usize> {
    if !source.contains("LocalListener::bind") {
        return Vec::new();
    }
    let lines = source.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("LocalListener::bind"))
        .filter(|(index, _)| needs_a_runtime(&lines, *index))
        .map(|(index, _)| index + 1)
        .collect()
}

fn needs_a_runtime(lines: &[&str], bind_index: usize) -> bool {
    let Some(declaration) = enclosing_function(lines, bind_index) else {
        return false;
    };
    if !is_plain_test(lines, declaration) {
        return false;
    }
    // An explicit runtime built inside the test body is a valid entry point.
    !lines[declaration..bind_index]
        .iter()
        .any(|line| line.contains("block_on") || line.contains(".enter()"))
}

fn enclosing_function(lines: &[&str], bind_index: usize) -> Option<usize> {
    (0..=bind_index)
        .rev()
        .find(|index| function_name(lines[*index]).is_some())
}

/// Returns `true` when the function is attributed `#[test]` and not
/// `#[tokio::test]`. Non-test call sites are the daemon's, which always binds
/// from inside its runtime.
fn is_plain_test(lines: &[&str], declaration: usize) -> bool {
    let mut plain = false;
    for line in lines[..declaration].iter().rev() {
        let trimmed = line.trim();
        if trimmed.contains("tokio::test") {
            return false;
        }
        if trimmed.starts_with("#[test") {
            plain = true;
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("#[") || trimmed.starts_with("//") {
            continue;
        }
        break;
    }
    plain
}

const FUNCTION_MODIFIERS: [&str; 5] = ["pub", "async", "const", "unsafe", "extern"];

fn function_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let start = if trimmed.starts_with("fn ") {
        0
    } else {
        trimmed.find(" fn ")? + 1
    };
    let modifiers_only = trimmed[..start].split_whitespace().all(|word| {
        let word = word.split('(').next().unwrap_or(word);
        FUNCTION_MODIFIERS.contains(&word) || word.starts_with('"')
    });
    if !modifiers_only {
        return None;
    }
    let name = trimmed[start + "fn ".len()..].trim_start();
    Some(name.split(['(', '<']).next().unwrap_or(name))
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if !matches!(name.to_str(), Some("target" | ".git" | "node_modules")) {
                    pending.push(path);
                }
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                sources.push(path);
            }
        }
    }
    sources
}

fn relative(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
