#![cfg(target_os = "linux")]

mod common;

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{acquire_empty_socket_path_lock, assert_success, CliHarness};

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        common::terminate_child(&mut self.0).expect("reap owned child");
    }
}

fn wait_file(path: &Path, child: &mut Child) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(child.try_wait()?.is_none(), "child exited before {path:?}");
        assert!(Instant::now() < deadline, "child did not publish {path:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn empty_socket_lock_spans_worktrees_and_cleanup() -> Result<(), Box<dyn Error>> {
    let lock = acquire_empty_socket_path_lock()?;
    let harness = CliHarness::new("empty-lock-owner")?;
    let root = harness.tmpdir().join("contender");
    fs::create_dir_all(root.join("tmp"))?;
    let mut daemon = ReapedChild(
        harness
            .base_command()
            .args([rmux_client::INTERNAL_DAEMON_FLAG, ""])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    common::wait_for_socket(Path::new(""), &mut daemon.0)?;
    assert_success(&harness.run(&["-S", "", "new-session", "-d", "-s", "lock_owner"])?);

    // This is a new process, not another thread sharing CliHarness's mutex.
    // Both its cwd and every temporary-root override differ from the owner.
    let mut contender = ReapedChild(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "empty_socket_lock_contender",
                "--ignored",
                "--nocapture",
            ])
            .current_dir(&root)
            .env("RMUX_EMPTY_LOCK_CONTENDER", &root)
            .env("TMPDIR", root.join("tmp"))
            .env("RMUX_TMPDIR", root.join("tmp"))
            .env("RMUX_TEST_TMPDIR", root.join("tmp"))
            .stdin(Stdio::null())
            .spawn()?,
    );
    wait_file(&root.join("attempting"), &mut contender.0)?;
    std::thread::sleep(Duration::from_millis(500));
    let acquired_while_owned = root.join("acquired").exists();
    let owner_still_alive = harness.run(&["-S", "", "has-session", "-t", "lock_owner"])?;

    // The guard must shut down the endpoint before handing it to the next
    // process, including when the test exits without an explicit kill-server.
    drop(lock);
    wait_file(&root.join("finished"), &mut contender.0)?;
    assert!(contender.0.wait()?.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon.0.try_wait()?.is_none() {
        assert!(Instant::now() < deadline, "owner daemon was not reaped");
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        !acquired_while_owned,
        "different cwd/TMPDIR acquired the shared empty endpoint lock"
    );
    assert_success(&owner_still_alive);
    assert_eq!(fs::read_to_string(root.join("joined_owner"))?, "false");
    assert_eq!(fs::read_to_string(root.join("killed_owner"))?, "false");
    Ok(())
}

#[test]
#[ignore = "subprocess of empty_socket_lock_spans_worktrees_and_cleanup"]
fn empty_socket_lock_contender() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(std::env::var_os("RMUX_EMPTY_LOCK_CONTENDER").expect("child only"));
    fs::write(root.join("attempting"), "ready")?;
    let _lock = acquire_empty_socket_path_lock()?;
    assert!(matches!(
        rmux_client::connect_or_absent(Path::new(""))?,
        rmux_client::ConnectResult::Absent
    ));
    fs::write(root.join("acquired"), "acquired")?;
    let harness = CliHarness::new("empty-lock-contender")?;
    let joined = harness.run(&["-S", "", "has-session", "-t", "lock_owner"])?;
    fs::write(
        root.join("joined_owner"),
        joined.status.success().to_string(),
    )?;
    let killed = harness.run(&["-S", "", "kill-server"])?;
    fs::write(
        root.join("killed_owner"),
        killed.status.success().to_string(),
    )?;
    fs::write(root.join("finished"), "finished")?;
    Ok(())
}
