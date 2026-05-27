#![cfg(windows)]

use super::TerminalProfile;
use rmux_core::{EnvironmentStore, OptionStore};
use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn terminal_profile_resolves_default_shell_with_command_environment_overrides() {
    let root = unique_directory("shell-env-override");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("test bin directory");
    fs::write(bin.join("profile-shell.exe"), b"").expect("shell fixture");
    let path = std::env::join_paths([bin.as_os_str()]).expect("joined PATH");
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "profile-shell".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");
    let session_name = SessionName::new("alpha").expect("valid session name");

    let profile = TerminalProfile::for_session(
        &EnvironmentStore::new(),
        &options,
        &session_name,
        7,
        Path::new(r"\\.\pipe\rmux-test"),
        true,
        Some(&[
            format!("PATH={}", path.to_string_lossy()),
            "PATHEXT=.EXE".to_owned(),
        ]),
        None,
        Some(root.as_path()),
        None,
    )
    .expect("profile");

    assert_eq!(profile.shell(), bin.join("profile-shell.EXE").as_path());
    let _ = fs::remove_dir_all(root);
}

fn unique_directory(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rmux-profile-env-{label}-{}-{unique_id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    path
}
