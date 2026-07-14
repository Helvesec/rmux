#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const EXECUTABLES: [(&str, u32); 3] = [
    ("libexec/rmux/rmux", 0o701),
    ("bin/rmux-daemon", 0o711),
    ("bin/rmux", 0o751),
];

const FAILURE_CHECKPOINTS: [&str; 11] = [
    "after-preflight",
    "after-stage-helper",
    "after-stage-daemon",
    "after-stage-tiny",
    "after-backup-helper",
    "after-backup-daemon",
    "after-backup-tiny",
    "after-replace-helper",
    "after-replace-daemon",
    "after-replace-tiny",
    "after-verify",
];

const SIGNAL_CHECKPOINTS: [&str; 4] = [
    "after-replace-helper",
    "after-replace-daemon",
    "after-replace-tiny",
    "after-verify",
];

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rmux-unix-installer-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create installer fixture root");
        Self(path)
    }

    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct FileSnapshot {
    bytes: Vec<u8>,
    mode: u32,
}

fn write_file(path: &Path, contents: &[u8], mode: u32) {
    fs::create_dir_all(path.parent().expect("fixture file parent"))
        .expect("create fixture directory");
    fs::write(path, contents).expect("write fixture file");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).expect("set fixture mode");
}

fn setup_archive(root: &TempRoot) -> PathBuf {
    let archive = root.join("archive");
    fs::create_dir_all(&archive).expect("create archive root");
    let source_script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/install-unix-archive.sh");
    let installer = archive.join("install.sh");
    fs::copy(source_script, &installer).expect("copy Unix archive installer");
    let mut permissions = fs::metadata(&installer)
        .expect("installer metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&installer, permissions).expect("make installer executable");

    write_valid_archive_binaries(&archive);
    write_file(&archive.join("share/rmux/version"), b"new-assets\n", 0o644);
    archive
}

fn write_valid_archive_binaries(archive: &Path) {
    write_file(
        &archive.join("libexec/rmux/rmux"),
        b"#!/bin/sh\n# rmux-helper:new\nexit 0\n",
        0o755,
    );
    write_file(
        &archive.join("bin/rmux-daemon"),
        b"#!/bin/sh\n# rmux-daemon:new\nexit 0\n",
        0o755,
    );
    write_file(
        &archive.join("bin/rmux"),
        br##"#!/bin/sh
helper="$(CDPATH= cd -- "$(dirname -- "$0")/../libexec/rmux" && pwd)/rmux"
if [ "${1:-}" = "--help" ] && grep -q '^# rmux-helper:new$' "$helper"; then
  printf 'usage: rmux\n'
  exit 0
fi
exit 42
"##,
        0o755,
    );
}

fn setup_old_layout(prefix: &Path) {
    for (index, (relative, mode)) in EXECUTABLES.iter().enumerate() {
        write_file(
            &prefix.join(relative),
            format!("#!/bin/sh\n# old-component-{index}\nexit 0\n").as_bytes(),
            *mode,
        );
    }
}

fn snapshot_layout(prefix: &Path) -> Vec<FileSnapshot> {
    EXECUTABLES
        .iter()
        .map(|(relative, _)| {
            let path = prefix.join(relative);
            FileSnapshot {
                bytes: fs::read(&path).expect("read installed executable"),
                mode: fs::metadata(&path)
                    .expect("installed executable metadata")
                    .permissions()
                    .mode()
                    & 0o7777,
            }
        })
        .collect()
}

fn run_installer(
    archive: &Path,
    prefix: &Path,
    fail_at: Option<&str>,
    signal_at: Option<&str>,
) -> Output {
    let mut command = Command::new(archive.join("install.sh"));
    command
        .args(["--prefix"])
        .arg(prefix)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("RMUX_INSTALL_TEST_FAIL_AT")
        .env_remove("RMUX_INSTALL_TEST_SIGNAL_AT");
    if let Some(checkpoint) = fail_at {
        command.env("RMUX_INSTALL_TEST_FAIL_AT", checkpoint);
    }
    if let Some(checkpoint) = signal_at {
        command.env("RMUX_INSTALL_TEST_SIGNAL_AT", checkpoint);
    }
    command.output().expect("run Unix archive installer")
}

fn output_details(output: &Output) -> String {
    format!(
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_old_layout_restored(prefix: &Path, expected: &[FileSnapshot], checkpoint: &str) {
    assert_eq!(
        snapshot_layout(prefix),
        expected,
        "old layout changed after {checkpoint}"
    );
    assert_no_transaction_residue(prefix);
}

fn assert_no_transaction_residue(root: &Path) {
    if !root.exists() {
        return;
    }
    for entry in fs::read_dir(root).expect("read install directory") {
        let entry = entry.expect("read install entry");
        let name = entry.file_name();
        assert!(
            !name.to_string_lossy().starts_with(".rmux-"),
            "installer residue remained at {}",
            entry.path().display()
        );
        if entry.file_type().expect("install entry type").is_dir() {
            assert_no_transaction_residue(&entry.path());
        }
    }
}

fn assert_new_layout(prefix: &Path) {
    for (relative, _) in EXECUTABLES {
        let path = prefix.join(relative);
        let metadata = fs::metadata(&path).expect("new executable metadata");
        assert!(
            metadata.is_file(),
            "missing new executable: {}",
            path.display()
        );
        assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
    }
    let help = Command::new(prefix.join("bin/rmux"))
        .arg("--help")
        .output()
        .expect("run installed tiny dispatcher");
    assert!(help.status.success(), "{}", output_details(&help));
    assert!(String::from_utf8_lossy(&help.stdout).contains("usage: rmux"));
    assert_eq!(
        fs::read(prefix.join("share/rmux/version")).expect("read installed asset"),
        b"new-assets\n"
    );
    assert_no_transaction_residue(prefix);
}

#[test]
fn upgrade_failures_restore_every_old_binary_and_allow_a_clean_retry() {
    let root = TempRoot::new("upgrade-failures");
    let archive = setup_archive(&root);

    for checkpoint in FAILURE_CHECKPOINTS {
        let prefix = root.join(format!("upgrade-{checkpoint}"));
        setup_old_layout(&prefix);
        let old = snapshot_layout(&prefix);

        let failed = run_installer(&archive, &prefix, Some(checkpoint), None);
        assert!(!failed.status.success(), "{}", output_details(&failed));
        assert_old_layout_restored(&prefix, &old, checkpoint);

        let retry = run_installer(&archive, &prefix, None, None);
        assert!(retry.status.success(), "{}", output_details(&retry));
        assert_new_layout(&prefix);
    }
}

#[test]
fn fresh_install_failures_remove_partial_layout_and_allow_a_clean_retry() {
    let root = TempRoot::new("fresh-failures");
    let archive = setup_archive(&root);

    for checkpoint in FAILURE_CHECKPOINTS {
        let prefix = root.join(format!("fresh-{checkpoint}"));
        let failed = run_installer(&archive, &prefix, Some(checkpoint), None);
        assert!(!failed.status.success(), "{}", output_details(&failed));
        assert!(
            !prefix.exists(),
            "fresh partial layout remained after {checkpoint}: {}",
            prefix.display()
        );

        let retry = run_installer(&archive, &prefix, None, None);
        assert!(retry.status.success(), "{}", output_details(&retry));
        assert_new_layout(&prefix);
    }
}

#[test]
fn handled_signals_restore_the_old_layout_through_verification() {
    let root = TempRoot::new("signals");
    let archive = setup_archive(&root);

    for checkpoint in SIGNAL_CHECKPOINTS {
        let prefix = root.join(format!("signal-{checkpoint}"));
        setup_old_layout(&prefix);
        let old = snapshot_layout(&prefix);

        let interrupted = run_installer(&archive, &prefix, None, Some(checkpoint));
        assert!(
            !interrupted.status.success(),
            "{}",
            output_details(&interrupted)
        );
        assert_old_layout_restored(&prefix, &old, checkpoint);
    }
}

#[test]
fn real_layout_verification_failure_rolls_back_before_assets_change() {
    let root = TempRoot::new("verify-failure");
    let archive = setup_archive(&root);
    let prefix = root.join("prefix");
    setup_old_layout(&prefix);
    let old = snapshot_layout(&prefix);
    write_file(
        &archive.join("libexec/rmux/rmux"),
        b"#!/bin/sh\n# rmux-helper:incompatible\nexit 0\n",
        0o755,
    );

    let failed = run_installer(&archive, &prefix, None, None);
    assert!(!failed.status.success(), "{}", output_details(&failed));
    assert_old_layout_restored(&prefix, &old, "real verification failure");
    assert!(!prefix.join("share/rmux/version").exists());

    write_valid_archive_binaries(&archive);
    let retry = run_installer(&archive, &prefix, None, None);
    assert!(retry.status.success(), "{}", output_details(&retry));
    assert_new_layout(&prefix);
}

#[test]
fn preflight_rejects_a_non_file_destination_before_other_binaries_change() {
    let root = TempRoot::new("preflight");
    let archive = setup_archive(&root);
    let prefix = root.join("prefix");
    setup_old_layout(&prefix);
    fs::remove_file(prefix.join("bin/rmux-daemon")).expect("remove old daemon fixture");
    fs::create_dir(prefix.join("bin/rmux-daemon")).expect("create invalid daemon directory");
    let helper_before = fs::read(prefix.join("libexec/rmux/rmux")).expect("read old helper");
    let tiny_before = fs::read(prefix.join("bin/rmux")).expect("read old tiny");

    let failed = run_installer(&archive, &prefix, None, None);
    assert!(!failed.status.success(), "{}", output_details(&failed));
    assert_eq!(
        fs::read(prefix.join("libexec/rmux/rmux")).expect("read helper after preflight"),
        helper_before
    );
    assert_eq!(
        fs::read(prefix.join("bin/rmux")).expect("read tiny after preflight"),
        tiny_before
    );
    assert!(prefix.join("bin/rmux-daemon").is_dir());
    assert_no_transaction_residue(&prefix);
}

#[test]
fn rollback_preserves_an_existing_dispatcher_symlink() {
    let root = TempRoot::new("symlink-rollback");
    let archive = setup_archive(&root);
    let prefix = root.join("prefix");
    setup_old_layout(&prefix);
    let tiny = prefix.join("bin/rmux");
    fs::remove_file(&tiny).expect("remove regular tiny fixture");
    write_file(
        &prefix.join("bin/old-rmux-real"),
        b"#!/bin/sh\n# old symlink target\nexit 0\n",
        0o755,
    );
    symlink("old-rmux-real", &tiny).expect("create old dispatcher symlink");

    let failed = run_installer(&archive, &prefix, Some("after-replace-tiny"), None);
    assert!(!failed.status.success(), "{}", output_details(&failed));
    assert!(fs::symlink_metadata(&tiny)
        .expect("restored tiny metadata")
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_link(&tiny).expect("read restored tiny symlink"),
        PathBuf::from("old-rmux-real")
    );
    assert_no_transaction_residue(&prefix);
}
