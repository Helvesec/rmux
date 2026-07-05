#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, CliHarness};

#[track_caller]
fn assert_status_success(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected successful command, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    assert!(stderr(output).is_empty(), "stderr should be empty");
}

#[test]
fn custom_socket_parent_0775_is_tmux_compatible_but_socket_stays_private(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("socket-parent-0775")?;
    let parent = harness.tmpdir().join("project");
    fs::create_dir_all(&parent)?;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o775))?;
    let socket = parent.join("rmux.sock");
    let socket_arg = socket.to_string_lossy().into_owned();

    let created = harness.run(&[
        "-S",
        socket_arg.as_str(),
        "new-session",
        "-d",
        "-s",
        "compat",
    ])?;
    assert_success(&created);
    assert_eq!(stderr(&created), "");

    let listed = harness.run(&[
        "-S",
        socket_arg.as_str(),
        "list-sessions",
        "-F",
        "#{session_name}",
    ])?;
    assert_status_success(&listed);
    assert_eq!(stdout(&listed).trim(), "compat");

    let metadata = fs::symlink_metadata(&socket)?;
    assert_eq!(
        metadata.mode() & 0o777,
        0o600,
        "bound custom sockets should still be owner-only"
    );

    let killed = harness.run(&["-S", socket_arg.as_str(), "kill-server"])?;
    assert_success(&killed);
    Ok(())
}

#[test]
fn socket_label_under_group_writable_tmpdir_still_uses_managed_private_dir(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("socket-label-0775")?;
    let root = harness.tmpdir().join("labels");
    fs::create_dir_all(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o775))?;

    let created = harness.run_with(
        &["-L", "dropin", "new-session", "-d", "-s", "label"],
        |cmd| {
            cmd.env("RMUX_TMPDIR", &root);
        },
    )?;
    assert_success(&created);
    assert_eq!(stderr(&created), "");

    let listed = harness.run_with(
        &["-L", "dropin", "list-sessions", "-F", "#{session_name}"],
        |cmd| {
            cmd.env("RMUX_TMPDIR", &root);
        },
    )?;
    assert_status_success(&listed);
    assert_eq!(stdout(&listed).trim(), "label");

    let user_id = unsafe { libc::geteuid() };
    let managed = root.join(format!("rmux-{user_id}"));
    let metadata = fs::symlink_metadata(&managed)?;
    assert_eq!(metadata.mode() & 0o777, 0o700);

    let killed = harness.run_with(&["-L", "dropin", "kill-server"], |cmd| {
        cmd.env("RMUX_TMPDIR", &root);
    })?;
    assert_success(&killed);
    Ok(())
}

#[test]
fn custom_socket_missing_parent_is_created_owner_only() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("socket-missing-parent")?;
    let parent = harness.tmpdir().join("missing").join("nested");
    let socket = parent.join("rmux.sock");
    let socket_arg = socket.to_string_lossy().into_owned();

    let created = harness.run(&[
        "-S",
        socket_arg.as_str(),
        "new-session",
        "-d",
        "-s",
        "created",
    ])?;
    assert_success(&created);
    assert_eq!(stderr(&created), "");

    let metadata = fs::symlink_metadata(&parent)?;
    assert_eq!(metadata.mode() & 0o777, 0o700);

    let killed = harness.run(&["-S", socket_arg.as_str(), "kill-server"])?;
    assert_success(&killed);
    Ok(())
}

#[test]
fn bare_relative_custom_socket_uses_current_directory() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("socket-bare-relative")?;
    let cwd = harness.tmpdir().join("cwd");
    fs::create_dir_all(&cwd)?;
    let socket = cwd.join("bare.sock");

    let created = harness.run_with(
        &["-S", "bare.sock", "new-session", "-d", "-s", "relative"],
        |cmd| {
            cmd.current_dir(&cwd);
        },
    )?;
    assert_success(&created);
    assert_eq!(stderr(&created), "");

    let listed = harness.run_with(
        &["-S", "bare.sock", "list-sessions", "-F", "#{session_name}"],
        |cmd| {
            cmd.current_dir(&cwd);
        },
    )?;
    assert_status_success(&listed);
    assert_eq!(stdout(&listed).trim(), "relative");

    let metadata = fs::symlink_metadata(&socket)?;
    assert_eq!(
        metadata.mode() & 0o777,
        0o600,
        "bare relative -S sockets should still be owner-only"
    );

    let killed = harness.run_with(&["-S", "bare.sock", "kill-server"], |cmd| {
        cmd.current_dir(&cwd);
    })?;
    assert_success(&killed);
    wait_for_path_absent(&socket)?;
    Ok(())
}

#[test]
fn version_branding_stays_rmux_for_rmux_binary_and_tmux_for_shim() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("version-shim")?;
    let rmux = harness.run(&["-V"])?;
    assert_status_success(&rmux);
    assert!(
        stdout(&rmux).starts_with("rmux "),
        "stdout={}",
        stdout(&rmux)
    );

    let shim = harness.tmpdir().join("tmux");
    symlink(env!("CARGO_BIN_EXE_rmux"), &shim)?;
    let tmux = Command::new(&shim).arg("-V").output()?;
    assert_status_success(&tmux);
    assert_eq!(stdout(&tmux), "tmux 3.4\n");
    Ok(())
}

#[test]
fn doctor_tmux_dropin_reports_missing_shim_and_suggests_setup() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("doctor-tmux-dropin")?;

    let output = harness.run(&["doctor", "tmux-dropin"])?;

    assert_status_success(&output);
    let rendered = stdout(&output);
    assert!(rendered.contains("shim:        not detected   (argv[0]=rmux)"));
    assert!(rendered.contains("suggested:   ln -s $(command -v rmux) ~/.local/bin/tmux"));
    assert!(rendered.contains("setup:       rmux setup tmux-shim"));
    Ok(())
}

#[test]
fn setup_tmux_shim_creates_local_bin_symlink() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("setup-tmux-shim")?;
    let home = harness.tmpdir().join("home");
    fs::create_dir_all(&home)?;

    let output = harness.run_with(&["setup", "tmux-shim"], |cmd| {
        cmd.env("HOME", &home);
    })?;

    assert_status_success(&output);
    assert!(stdout(&output).contains("created:"));
    let shim = home.join(".local").join("bin").join("tmux");
    let target = fs::read_link(&shim)?;
    assert_eq!(
        fs::canonicalize(target)?,
        fs::canonicalize(env!("CARGO_BIN_EXE_rmux"))?
    );

    let tmux = Command::new(&shim).arg("-V").output()?;
    assert_status_success(&tmux);
    assert_eq!(stdout(&tmux), "tmux 3.4\n");
    Ok(())
}

#[test]
fn explicit_dev_null_config_is_silent_and_not_recorded_as_config_error(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("dev-null-config")?;
    let created = harness.run(&["-f", "/dev/null", "new-session", "-d", "-s", "nullcfg"])?;
    assert_success(&created);
    assert_eq!(stderr(&created), "");

    let messages = harness.run(&["show-messages"])?;
    let combined = format!("{}{}", stdout(&messages), stderr(&messages));
    assert!(
        !combined.contains("/dev/null") && !combined.contains("config error"),
        "unexpected config message: {combined:?}"
    );
    Ok(())
}

fn wait_for_path_absent(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while path.exists() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for '{}' to disappear", path.display()).into());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}
