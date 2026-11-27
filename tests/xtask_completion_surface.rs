//! The completion generator must build and run outside `cfg(test)`.
//!
//! `xtask` includes the shared CLI argument source under `#[cfg(not(test))]`,
//! and `completions::generate` is stubbed out under `cfg(test)`. xtask's own
//! unit tests therefore cannot observe a crate-root dependency that the shared
//! source needs, so a missing shared module stays invisible until something
//! builds the real binary. This spec builds and runs that binary.

#[cfg(unix)]
#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A nested cargo invocation needs its own output directory: the outer test
/// run owns the ambient one.
fn scratch_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()))
}

fn describe(output: &Output) -> String {
    format!(
        "status: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn generate_bash_completion(output_dir: &Path, nested_target_dir: &Path) -> Output {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    Command::new(cargo)
        .args([
            "run",
            "--locked",
            "--offline",
            "--quiet",
            "-p",
            "xtask",
            "--",
        ])
        .arg("generate-completions")
        .arg("--output-dir")
        .arg(output_dir)
        .args(["--shell", "bash"])
        .current_dir(workspace_root())
        // Cargo must assign each compiled crate its own build-script output.
        .env_remove("OUT_DIR")
        .env("CARGO_TARGET_DIR", nested_target_dir)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("cargo run -p xtask must be spawnable")
}

#[test]
fn xtask_generates_completions_from_the_shared_cli_surface() {
    let scratch = scratch_dir("xtask-completion-surface");
    let output_dir = scratch.join("completions");
    let nested_target_dir = scratch.join("target");
    let _ = fs::remove_dir_all(&output_dir);

    let output = generate_bash_completion(&output_dir, &nested_target_dir);
    assert!(
        output.status.success(),
        "the non-test xtask binary must build and generate completions\n{}",
        describe(&output),
    );

    let generated = output_dir.join("rmux.bash");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rmux.bash"),
        "xtask must report the generated completion path\n{}",
        describe(&output),
    );

    let script = fs::read_to_string(&generated)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", generated.display()));

    // Shape produced by the generator itself.
    assert!(
        script.contains("_rmux()"),
        "missing bash completion function"
    );
    assert!(
        script.contains("complete -F _rmux"),
        "missing bash completion registration"
    );

    // Surface that can only come from the shared `cli_args` command owner, so a
    // stub or an empty command surface cannot satisfy this spec.
    for command in ["new-session", "kill-server"] {
        assert!(
            script.contains(command),
            "generated completions must expose the shared RMUX surface: {command}"
        );
    }
}

#[cfg(unix)]
#[test]
fn nested_cargo_does_not_inherit_the_parent_build_output() {
    let scratch = scratch_dir("xtask-completion-parent-output");
    fs::create_dir_all(&scratch).unwrap();
    let launcher = scratch.join("cargo");
    executable_fixture::write_executable(
        &launcher,
        "#!/bin/sh\n\
         if [ \"${OUT_DIR+x}\" = x ]; then\n\
           echo 'nested Cargo inherited its parent build output' >&2\n\
           exit 86\n\
         fi\n\
         exec \"$RMUX_TEST_REAL_CARGO\" \"$@\"\n",
        0o755,
    )
    .unwrap();

    // Re-enter the existing acceptance test in a fresh process, so injection
    // cannot race with other tests or remove their legitimate build context.
    let executable = std::env::current_exe().unwrap();
    let executable = rmux_os::build_identity::resolve(executable.as_os_str()).unwrap();
    let output = Command::new(executable)
        .args([
            "--exact",
            "xtask_generates_completions_from_the_shared_cli_surface",
            "--nocapture",
        ])
        .env("CARGO", launcher)
        .env(
            "RMUX_TEST_REAL_CARGO",
            std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
        )
        .env("OUT_DIR", scratch.join("parent-build-output"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "nested Cargo must own its build output and still generate completions\n{}",
        describe(&output),
    );
}
