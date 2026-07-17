#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EXECUTABLES: [(&str, u32); 3] = [
    ("libexec/rmux/rmux", 0o701),
    ("bin/rmux-daemon", 0o711),
    ("bin/rmux", 0o751),
];

const PACKAGE_ASSETS: [(&str, &[u8], &[u8]); 2] = [
    ("share/rmux/version", b"old-assets\n", b"new-assets\n"),
    (
        "share/man/man1/rmux.1",
        b"old-man-page\n",
        b"new-man-page\n",
    ),
];
const LOCAL_ASSET: (&str, &[u8]) = ("share/rmux/local-only", b"keep-local-asset\n");

const FAILURE_CHECKPOINTS: [&str; 12] = [
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
    "after-replace-asset",
];

const SIGNAL_CHECKPOINTS: [&str; 5] = [
    "after-replace-helper",
    "after-replace-daemon",
    "after-replace-tiny",
    "after-verify",
    "after-replace-asset",
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
    setup_named_archive(root, "archive")
}

fn setup_named_archive(root: &TempRoot, name: &str) -> PathBuf {
    let archive = root.join(name);
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
    for (relative, _, contents) in PACKAGE_ASSETS {
        write_file(&archive.join(relative), contents, 0o644);
    }
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

fn setup_old_assets(prefix: &Path) {
    for (relative, contents, _) in PACKAGE_ASSETS {
        write_file(&prefix.join(relative), contents, 0o640);
    }
    write_file(&prefix.join(LOCAL_ASSET.0), LOCAL_ASSET.1, 0o600);
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

fn snapshot_old_assets(prefix: &Path) -> Vec<FileSnapshot> {
    PACKAGE_ASSETS
        .iter()
        .map(|(relative, _, _)| *relative)
        .chain(std::iter::once(LOCAL_ASSET.0))
        .map(|relative| {
            let path = prefix.join(relative);
            FileSnapshot {
                bytes: fs::read(&path).expect("read installed asset"),
                mode: fs::metadata(&path)
                    .expect("installed asset metadata")
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
    let mut command = installer_command(archive, prefix);
    if let Some(checkpoint) = fail_at {
        command.env("RMUX_INSTALL_TEST_FAIL_AT", checkpoint);
    }
    if let Some(checkpoint) = signal_at {
        command.env("RMUX_INSTALL_TEST_SIGNAL_AT", checkpoint);
    }
    command.output().expect("run Unix archive installer")
}

fn installer_command(archive: &Path, prefix: &Path) -> Command {
    let mut command = Command::new("/usr/bin/env");
    command
        .arg("bash")
        .arg(archive.join("install.sh"))
        .args(["--prefix"])
        .arg(prefix)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("RMUX_INSTALL_TEST_LOCK_WAIT_FILE")
        .env_remove("RMUX_INSTALL_TEST_LOCK_ACQUIRED_FILE")
        .env_remove("RMUX_INSTALL_TEST_LOCK_RESUME_FILE")
        .env_remove("RMUX_INSTALL_TEST_VERIFY_STARTED")
        .env_remove("RMUX_INSTALL_TEST_VERIFY_RESUME")
        .env_remove("RMUX_INSTALL_TEST_FAIL_AT")
        .env_remove("RMUX_INSTALL_TEST_SIGNAL_AT");
    command
}

fn wait_until_exists(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    path.exists()
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
    for (relative, _, expected) in PACKAGE_ASSETS {
        assert_eq!(
            fs::read(prefix.join(relative)).expect("read installed asset"),
            expected
        );
    }
    assert_no_transaction_residue(prefix);
}

#[test]
fn upgrade_failures_restore_every_old_binary_and_allow_a_clean_retry() {
    let root = TempRoot::new("upgrade-failures");
    let archive = setup_archive(&root);

    for checkpoint in FAILURE_CHECKPOINTS {
        let prefix = root.join(format!("upgrade-{checkpoint}"));
        setup_old_layout(&prefix);
        setup_old_assets(&prefix);
        let old = snapshot_layout(&prefix);
        let old_assets = snapshot_old_assets(&prefix);

        let failed = run_installer(&archive, &prefix, Some(checkpoint), None);
        assert!(!failed.status.success(), "{}", output_details(&failed));
        assert_old_layout_restored(&prefix, &old, checkpoint);
        assert_eq!(
            snapshot_old_assets(&prefix),
            old_assets,
            "old assets changed after {checkpoint}"
        );

        let retry = run_installer(&archive, &prefix, None, None);
        assert!(retry.status.success(), "{}", output_details(&retry));
        assert_new_layout(&prefix);
        assert_eq!(
            fs::read(prefix.join(LOCAL_ASSET.0)).expect("read preserved local asset"),
            LOCAL_ASSET.1
        );
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
        setup_old_assets(&prefix);
        let old = snapshot_layout(&prefix);
        let old_assets = snapshot_old_assets(&prefix);

        let interrupted = run_installer(&archive, &prefix, None, Some(checkpoint));
        assert!(
            !interrupted.status.success(),
            "{}",
            output_details(&interrupted)
        );
        assert_old_layout_restored(&prefix, &old, checkpoint);
        assert_eq!(
            snapshot_old_assets(&prefix),
            old_assets,
            "old assets changed after signal at {checkpoint}"
        );
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

#[test]
fn archive_without_assets_installs_without_transaction_residue() {
    let root = TempRoot::new("no-assets");
    let archive = setup_archive(&root);
    fs::remove_dir_all(archive.join("share")).expect("remove optional archive assets");
    let prefix = root.join("prefix");

    let installed = run_installer(&archive, &prefix, None, None);
    assert!(installed.status.success(), "{}", output_details(&installed));
    for (relative, _) in EXECUTABLES {
        assert!(prefix.join(relative).is_file(), "missing {relative}");
    }
    assert!(!prefix.join("share").exists());
    assert_no_transaction_residue(&prefix);
}

#[test]
fn sigkill_after_lock_owner_is_reclaimed_by_a_clean_retry() {
    let root = TempRoot::new("stale-lock");
    let archive = setup_archive(&root);
    let prefix = root.join("prefix");
    setup_old_layout(&prefix);
    setup_old_assets(&prefix);
    let old = snapshot_layout(&prefix);
    let old_assets = snapshot_old_assets(&prefix);
    let lock_acquired = root.join("lock-acquired");
    let never_resume = root.join("never-resume");

    let mut interrupted = installer_command(&archive, &prefix)
        .env("RMUX_INSTALL_TEST_LOCK_ACQUIRED_FILE", &lock_acquired)
        .env("RMUX_INSTALL_TEST_LOCK_RESUME_FILE", &never_resume)
        .spawn()
        .expect("spawn installer held after writing lock owner");
    if !wait_until_exists(&lock_acquired, Duration::from_secs(5)) {
        let _ = interrupted.kill();
        let _ = interrupted.wait();
        panic!("installer never reported writing its lock owner");
    }

    let lock_owner = prefix.join(".rmux-install.lock/owner");
    let owner = fs::read_to_string(&lock_owner).expect("read stale lock owner");
    let expected_owner_pid = interrupted.id().to_string();
    assert_eq!(
        fs::metadata(&lock_owner)
            .expect("stale lock owner metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "lock ownership must not be readable by other users"
    );
    assert_eq!(
        owner.lines().next(),
        Some(expected_owner_pid.as_str()),
        "lock owner should identify the installer process"
    );
    interrupted.kill().expect("SIGKILL held installer");
    let interrupted_status = interrupted.wait().expect("reap killed installer");
    assert!(!interrupted_status.success());
    assert_eq!(snapshot_layout(&prefix), old);
    assert_eq!(snapshot_old_assets(&prefix), old_assets);
    assert!(lock_owner.is_file(), "SIGKILL should leave a stale lock");

    let retry = run_installer(&archive, &prefix, None, None);
    assert!(retry.status.success(), "{}", output_details(&retry));
    assert_new_layout(&prefix);
}

#[test]
fn concurrent_install_waits_until_failed_rollback_is_complete() {
    let root = TempRoot::new("concurrent-rollback");
    let failing_archive = setup_named_archive(&root, "archive-failing");
    let succeeding_archive = setup_named_archive(&root, "archive-succeeding");
    let prefix = root.join("prefix");
    setup_old_layout(&prefix);
    setup_old_assets(&prefix);

    let verify_started = root.join("verify-started");
    let verify_resume = root.join("verify-resume");
    let lock_waiting = root.join("lock-waiting");
    write_file(
        &failing_archive.join("bin/rmux"),
        br##"#!/bin/sh
: > "${RMUX_INSTALL_TEST_VERIFY_STARTED:?}"
while [ ! -e "${RMUX_INSTALL_TEST_VERIFY_RESUME:?}" ]; do
  sleep 0.01
done
exit 42
"##,
        0o755,
    );

    let (first_tx, first_rx) = mpsc::channel();
    let first_archive = failing_archive.clone();
    let first_prefix = prefix.clone();
    let first_started = verify_started.clone();
    let first_resume = verify_resume.clone();
    let first_thread = thread::spawn(move || {
        let output = installer_command(&first_archive, &first_prefix)
            .env("RMUX_INSTALL_TEST_VERIFY_STARTED", first_started)
            .env("RMUX_INSTALL_TEST_VERIFY_RESUME", first_resume)
            .output()
            .expect("run failing concurrent installer");
        first_tx
            .send(output)
            .expect("report first installer output");
    });

    let first_reached_verify = wait_until_exists(&verify_started, Duration::from_secs(5));
    let live_owner_before = first_reached_verify.then(|| {
        fs::read(prefix.join(".rmux-install.lock/owner")).expect("read live installer lock owner")
    });

    let (second_tx, second_rx) = mpsc::channel();
    let second_archive = succeeding_archive.clone();
    let second_prefix = prefix.clone();
    let second_waiting = lock_waiting.clone();
    let second_thread = thread::spawn(move || {
        let output = installer_command(&second_archive, &second_prefix)
            .env("RMUX_INSTALL_TEST_LOCK_WAIT_FILE", second_waiting)
            .output()
            .expect("run succeeding concurrent installer");
        second_tx
            .send(output)
            .expect("report second installer output");
    });

    let wait_deadline = Instant::now() + Duration::from_secs(5);
    let mut second_early = None;
    let mut second_waited = false;
    while Instant::now() < wait_deadline {
        if lock_waiting.exists() {
            second_waited = true;
            break;
        }
        match second_rx.try_recv() {
            Ok(output) => {
                second_early = Some(output);
                break;
            }
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(TryRecvError::Disconnected) => break,
        }
    }
    if let Some(live_owner_before) = live_owner_before {
        assert_eq!(
            fs::read(prefix.join(".rmux-install.lock/owner"))
                .expect("read live lock after concurrent waiter"),
            live_owner_before,
            "a concurrent installer must not reclaim a live owner's lock"
        );
    }

    write_file(&verify_resume, b"resume\n", 0o600);
    let first = first_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("failing installer should finish after release");
    let second = second_early.unwrap_or_else(|| {
        second_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("waiting installer should finish after rollback")
    });
    first_thread.join().expect("join failing installer thread");
    second_thread
        .join()
        .expect("join succeeding installer thread");

    assert!(
        first_reached_verify,
        "first installer never reached its verification barrier: {}",
        output_details(&first)
    );
    assert!(
        second_waited,
        "second installer did not wait for the active transaction: {}",
        output_details(&second)
    );
    assert!(!first.status.success(), "{}", output_details(&first));
    assert!(second.status.success(), "{}", output_details(&second));
    assert_new_layout(&prefix);
}
