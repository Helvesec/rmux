//! These checks exercise the selected child, after the outer Rust test starts.
#[cfg(unix)]
#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

static ENVIRONMENT: Mutex<()> = Mutex::new(());

/// Only a candidate identity build (scripts/candidate_identity.py) embeds the
/// head these children are checked against; a plain `cargo test` skips them.
fn identity_build() -> bool {
    let embedded = option_env!("RMUX_IDENTITY_HEAD").is_some();
    if !embedded {
        eprintln!("runtime skip: candidate identity build required (RMUX_IDENTITY_HEAD)");
    }
    embedded
}

fn bad_child(label: &str) -> (PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("tmp")
        .join(format!("identity-child-{}-{label}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let canary = root.join("entered");
    assert!(!canary.exists(), "fresh fixture required");
    #[cfg(unix)]
    let binary = {
        let path = root.join("unbound-child.sh");
        executable_fixture::write_executable(
            &path,
            format!(
                "#!/bin/sh\nprintf entered > '{}'\nexit 77\n",
                canary.display()
            ),
            0o755,
        )
        .unwrap();
        path
    };
    #[cfg(windows)]
    let binary = {
        let path = root.join("unbound-child.cmd");
        fs::write(
            &path,
            format!(
                "@echo off\r\necho entered>\"{}\"\r\nexit /b 77\r\n",
                canary.display()
            ),
        )
        .unwrap();
        path
    };
    (binary, canary)
}

fn endpoint(label: &str) -> PathBuf {
    #[cfg(unix)]
    {
        let path = std::env::temp_dir().join(format!("rmux-id-{}-{label}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path.join("s")
    }
    #[cfg(windows)]
    {
        let label = format!("identity-child-{}-{label}", std::process::id());
        rmux_client::resolve_socket_path(Some(std::ffi::OsStr::new(&label)), None)
            .expect("current-user Windows pipe")
    }
}

#[test]
fn cli_selected_daemon_without_identity_is_rejected_before_execution() {
    if !identity_build() {
        return;
    }
    let (binary, canary) = bad_child("cli");
    let output = Command::new(rmux_os::build_identity::cargo_binary(env!(
        "CARGO_BIN_EXE_rmux"
    )))
    .env("RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE", "1")
    .env("RMUX_INTERNAL_BINARY_PATH", &binary)
    .env_remove("RMUX")
    .env_remove("TMUX")
    .arg("-S")
    .arg(endpoint("cli"))
    .args([
        "-f",
        if cfg!(windows) { "NUL" } else { "/dev/null" },
        "new-session",
        "-d",
        "-s",
        "identity-child",
    ])
    .output()
    .unwrap();
    assert!(
        !canary.exists(),
        "unbound CLI daemon entered before rejection"
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("candidate executable identity"));
}

#[test]
fn sdk_late_daemon_override_without_identity_is_rejected_before_execution() {
    let _lock = ENVIRONMENT.lock().unwrap();
    if !identity_build() {
        return;
    }
    let (binary, canary) = bad_child("sdk");
    let old = std::env::var_os("RMUX_SDK_DAEMON_BINARY");
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &binary);
    let builder = rmux_sdk::RmuxBuilder::new().default_timeout(Duration::from_secs(3));
    #[cfg(unix)]
    let builder = builder.unix_socket(endpoint("sdk"));
    #[cfg(windows)]
    let builder = builder.windows_pipe(endpoint("sdk").to_string_lossy().into_owned());
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(builder.connect_or_start());
    if let Some(value) = old {
        std::env::set_var("RMUX_SDK_DAEMON_BINARY", value);
    } else {
        std::env::remove_var("RMUX_SDK_DAEMON_BINARY");
    }
    assert!(!canary.exists(), "late SDK daemon entered before rejection");
    assert!(result.is_err());
}
