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

#[test]
fn rust_source_collection_preserves_recursive_runtime_guard() {
    let root = source_fixture("recursive");
    let nested = root.join("crates/example/tests/nested");
    fs::create_dir_all(&nested).expect("create nested sources");
    let source = nested.join("runtime.rs");
    let plain = "#[test]\nfn binds() {\n    LocalListener::bind(path);\n}\n";
    fs::write(&source, plain).expect("write nested source");
    fs::write(nested.join("notes.txt"), plain).expect("write non-Rust file");
    // A directory with a Rust suffix must still be traversed.
    let dotted = root.join("module.rs");
    fs::create_dir(&dotted).expect("create Rust-suffixed directory");
    let neighbor = dotted.join("neighbor.rs");
    fs::write(&neighbor, "// recursive neighbor\n").expect("write neighbor");

    let mut sources = rust_sources(&root);
    sources.sort();
    let mut expected = vec![source.clone(), neighbor];
    expected.sort();
    assert_eq!(sources, expected);
    assert_eq!(
        bind_sites_without_a_runtime(&fs::read_to_string(source).expect("read nested source")),
        vec![3]
    );
    for allowed in [
        plain.replace("#[test]\nfn", "#[tokio::test]\nasync fn"),
        plain
            .replace(
                "    LocalListener",
                "    runtime.block_on(async {\n    LocalListener",
            )
            .replace(";\n}", ";\n    });\n}"),
        plain.replace(
            "    LocalListener",
            "    let _guard = runtime.enter();\n    LocalListener",
        ),
    ] {
        assert!(bind_sites_without_a_runtime(&allowed).is_empty());
    }
}

#[test]
fn rust_source_collection_excludes_generated_trees() {
    let root = source_fixture("generated");
    for excluded in ["target", ".git", "node_modules", ".local-records"] {
        let directory = root.join("nested").join(excluded).join("deeper");
        fs::create_dir_all(&directory).expect("create generated tree");
        fs::write(directory.join("generated.rs"), "// not workspace source\n")
            .expect("write generated source");
    }
    assert!(rust_sources(&root).is_empty());
}

#[cfg(unix)]
#[test]
fn rust_source_collection_ignores_symlinks_including_recursive_neighbor() {
    use std::os::unix::fs::symlink;

    let root = source_fixture("symlinks");
    let nested = root.join("nested");
    fs::create_dir(&nested).expect("create source directory");
    let source = nested.join("real.rs");
    fs::write(&source, "// real source\n").expect("write real source");
    symlink(".", nested.join("recursive")).expect("link recursive neighbor");
    symlink("nested", root.join("alias")).expect("link directory alias");
    symlink("nested/real.rs", root.join("alias.rs")).expect("link source alias");
    symlink("missing", root.join("dangling.rs")).expect("link dangling source");
    assert_eq!(rust_sources(&root), vec![source]);
}

fn source_fixture(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rmux-rust-sources-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).expect("create source fixture");
    root
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
            // Do not follow aliases or recursive links in generated fixtures.
            let file_type = entry.file_type().expect("read source entry type");
            if file_type.is_dir() {
                // Hidden directories (`.git`, local records) are never workspace source.
                let generated = name.to_str().is_some_and(|name| {
                    name.starts_with('.') || matches!(name, "target" | "node_modules")
                });
                if !generated {
                    pending.push(path);
                }
            } else if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            {
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
