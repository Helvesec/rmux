#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::process::{Child, Command, ExitStatus};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, terminate_child, CliHarness};

#[test]
fn load_buffer_reads_server_side_file() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let input_path = harness.tmpdir().join("input.txt");
    std::fs::write(&input_path, "loaded from file")?;

    assert_success(&harness.run(&[
        "load-buffer",
        "-b",
        "loaded",
        input_path.to_str().expect("utf-8 test path"),
    ])?);

    let show = harness.run(&["show-buffer", "-b", "loaded"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "loaded from file");
    assert!(stderr(&show).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn load_buffer_accepts_mixed_flag_order() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-flag-order")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let input_path = harness.tmpdir().join("input.txt");
    std::fs::write(&input_path, "loaded with mixed flags")?;

    assert_success(&harness.run(&[
        "load-buffer",
        "-w",
        "-b",
        "loaded",
        input_path.to_str().expect("utf-8 test path"),
    ])?);

    let show = harness.run(&["show-buffer", "-b", "loaded"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "loaded with mixed flags");
    assert!(stderr(&show).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn buffer_target_client_missing_is_a_successful_direct_cli_noop() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buffer-target-client-missing")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let input_path = harness.tmpdir().join("input.txt");
    fs::write(&input_path, "loaded despite missing client")?;

    assert_success(&harness.run(&[
        "set-buffer",
        "-w",
        "-t",
        "missing-client",
        "-b",
        "set-targeted",
        "set despite missing client",
    ])?);
    assert_success(&harness.run(&[
        "load-buffer",
        "-w",
        "-t",
        "missing-client",
        "-b",
        "load-targeted",
        input_path.to_str().expect("utf-8 test path"),
    ])?);
    assert_success(&run_with_stdin(
        &harness,
        &[
            "load-buffer",
            "-w",
            "-t",
            "missing-client",
            "-b",
            "stdin-targeted",
            "-",
        ],
        b"stdin despite missing client",
    )?);

    for (name, expected) in [
        ("set-targeted", "set despite missing client"),
        ("load-targeted", "loaded despite missing client"),
        ("stdin-targeted", "stdin despite missing client"),
    ] {
        let shown = harness.run(&["show-buffer", "-b", name])?;
        assert_eq!(shown.status.code(), Some(0));
        assert_eq!(stdout(&shown), expected);
        assert!(stderr(&shown).is_empty());
    }

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn load_buffer_reads_stdin_when_path_is_dash() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-dash")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let load = run_with_stdin(
        &harness,
        &["load-buffer", "-b", "dash", "-"],
        b"stdin bytes",
    )?;
    assert_success(&load);

    let show = harness.run(&["show-buffer", "-b", "dash"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "stdin bytes");
    assert!(stderr(&show).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn load_buffer_empty_stdin_is_successful_noop() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-dash-empty")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let load = run_with_stdin(&harness, &["load-buffer", "-b", "empty", "-"], b"")?;
    assert_success(&load);

    let show = harness.run(&["show-buffer", "-b", "empty"])?;
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("no buffer empty"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn load_buffer_empty_file_is_successful_noop() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-empty-file")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let input_path = harness.tmpdir().join("empty.txt");
    fs::write(&input_path, "")?;

    assert_success(&harness.run(&[
        "load-buffer",
        "-b",
        "empty",
        input_path.to_str().expect("utf-8 test path"),
    ])?);

    let show = harness.run(&["show-buffer", "-b", "empty"])?;
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("no buffer empty"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn official_daemon_observes_instantaneous_empty_fifo_writers() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-empty-fifo")?;
    let mut daemon = harness.start_hidden_daemon()?;

    for attempt in 0..50 {
        let fifo_path = harness.tmpdir().join(format!("empty-{attempt}.fifo"));
        let created = Command::new("mkfifo").arg(&fifo_path).output()?;
        assert!(
            created.status.success(),
            "mkfifo failed: {}",
            String::from_utf8_lossy(&created.stderr)
        );

        let mut load_command = harness.base_command();
        load_command
            .args([
                "load-buffer",
                "-b",
                "empty-fifo",
                fifo_path.to_str().expect("utf-8 FIFO path"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut load = load_command.spawn()?;
        let mut writer = Command::new("/bin/sh")
            .args(["-c", "exec 3>\"$1\"; exec 3>&-", "rmux-empty-fifo-writer"])
            .arg(&fifo_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let (load_status, writer_status) = loop {
            let load_status = load.try_wait()?;
            let writer_status = writer.try_wait()?;
            if let (Some(load_status), Some(writer_status)) = (load_status, writer_status) {
                break (load_status, writer_status);
            }
            if Instant::now() >= deadline {
                let _ = load.kill();
                let _ = writer.kill();
                let load_output = load.wait_with_output()?;
                let _ = writer.wait();
                return Err(format!(
                    "empty FIFO attempt {attempt} timed out: status={:?}, stderr={}",
                    load_output.status,
                    String::from_utf8_lossy(&load_output.stderr)
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(5));
        };

        let load_output = load.wait_with_output()?;
        assert_eq!(load_output.status, load_status);
        assert_success(&load_output);
        assert!(writer_status.success(), "empty FIFO writer failed");
        fs::remove_file(fifo_path)?;
    }

    let show = harness.run(&["show-buffer", "-b", "empty-fifo"])?;
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("no buffer empty-fifo"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn official_daemon_drains_fifo_payload_larger_than_helper_pipe() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-large-fifo")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let fifo_path = harness.tmpdir().join("large.fifo");
    let first_payload_path = harness.tmpdir().join("large-first.payload");
    let second_payload_path = harness.tmpdir().join("large-second.payload");
    let first = vec![b'a'; 128 * 1024];
    let second = vec![b'b'; 128 * 1024];
    let expected = [first.as_slice(), second.as_slice()].concat();
    fs::write(&first_payload_path, first)?;
    fs::write(&second_payload_path, second)?;
    assert_success(&Command::new("mkfifo").arg(&fifo_path).output()?);

    let mut load = harness
        .base_command()
        .args([
            "load-buffer",
            "-b",
            "large-fifo",
            fifo_path.to_str().expect("utf-8 FIFO path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut writer = Command::new("/bin/sh")
        .args([
            "-c",
            "exec 3>\"$1\"; cat \"$2\" >&3; sleep 0.15; cat \"$3\" >&3; exec 3>&-",
            "rmux-large-fifo-writer",
        ])
        .arg(&fifo_path)
        .arg(&first_payload_path)
        .arg(&second_payload_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let (load_status, writer_status) =
        wait_for_child_pair(&mut load, &mut writer, Duration::from_secs(10))?;
    let load_output = load.wait_with_output()?;
    let writer_output = writer.wait_with_output()?;
    assert_eq!(load_output.status, load_status);
    assert_eq!(writer_output.status, writer_status);
    assert_success(&load_output);
    assert_success(&writer_output);

    let shown = harness.run(&["show-buffer", "-b", "large-fifo"])?;
    assert_eq!(
        shown.status.code(),
        Some(0),
        "show-buffer failed: {}",
        stderr(&shown)
    );
    assert!(shown.stderr.is_empty(), "show-buffer wrote stderr");
    assert_eq!(shown.stdout, expected);

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn private_fifo_helper_rejects_replacement_after_classification() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("fifo-helper-replacement")?;
    let fifo_path = harness.tmpdir().join("classified.fifo");
    let original_path = harness.tmpdir().join("classified.original.fifo");
    assert_success(&Command::new("mkfifo").arg(&fifo_path).output()?);
    let metadata = fs::metadata(&fifo_path)?;
    fs::rename(&fifo_path, &original_path)?;
    assert_success(&Command::new("mkfifo").arg(&fifo_path).output()?);

    let mut helper = harness
        .base_command()
        .arg("--__internal-fifo-reader")
        .arg(&fifo_path)
        .arg(metadata.dev().to_string())
        .arg(metadata.ino().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut writer = Command::new("/bin/sh")
        .args([
            "-c",
            "exec 3>\"$1\"; exec 3>&-",
            "rmux-replacement-fifo-writer",
        ])
        .arg(&fifo_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let (helper_status, writer_status) =
        wait_for_child_pair(&mut helper, &mut writer, Duration::from_secs(10))?;
    let helper_output = helper.wait_with_output()?;
    let writer_output = writer.wait_with_output()?;
    assert_eq!(helper_output.status, helper_status);
    assert_eq!(writer_output.status, writer_status);
    assert_eq!(helper_output.status.code(), Some(65));
    assert!(helper_output.stdout.is_empty());
    assert!(stderr(&helper_output).contains("changed during blocking open"));
    assert_success(&writer_output);
    Ok(())
}

#[cfg(target_os = "macos")]
fn wait_for_child_pair(
    left: &mut Child,
    right: &mut Child,
    timeout: Duration,
) -> Result<(ExitStatus, ExitStatus), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let (Some(left_status), Some(right_status)) = (left.try_wait()?, right.try_wait()?) {
            return Ok((left_status, right_status));
        }
        if Instant::now() >= deadline {
            let _ = left.kill();
            let _ = right.kill();
            let _ = left.wait();
            let _ = right.wait();
            return Err("timed out waiting for FIFO helper process pair".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn save_buffer_writes_server_side_file() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("output.txt");

    assert_success(&harness.run(&["set-buffer", "-b", "saved", "save this"])?);
    assert_success(&harness.run(&[
        "save-buffer",
        "-b",
        "saved",
        output_path.to_str().expect("utf-8 test path"),
    ])?);

    assert_eq!(std::fs::read_to_string(&output_path)?, "save this");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn save_buffer_accepts_mixed_flag_order_and_appends() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer-flag-order")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("output.txt");

    std::fs::write(&output_path, "prefix:")?;
    assert_success(&harness.run(&["set-buffer", "-b", "saved", "tail"])?);
    assert_success(&harness.run(&[
        "save-buffer",
        "-a",
        "-b",
        "saved",
        output_path.to_str().expect("utf-8 test path"),
    ])?);

    assert_eq!(std::fs::read_to_string(&output_path)?, "prefix:tail");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn save_buffer_writes_stdout_when_path_is_dash() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer-dash")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "saved", "stdout bytes"])?);
    let save = harness.run(&["save-buffer", "-b", "saved", "-"])?;
    assert_eq!(save.status.code(), Some(0));
    assert_eq!(stdout(&save), "stdout bytes");
    assert!(stderr(&save).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn save_buffer_append_flag_still_writes_stdout_when_path_is_dash() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer-dash-append")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "saved", "append stdout"])?);
    let save = harness.run(&["save-buffer", "-a", "-b", "saved", "-"])?;
    assert_eq!(save.status.code(), Some(0));
    assert_eq!(stdout(&save), "append stdout");
    assert!(stderr(&save).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn load_buffer_resolves_relative_paths_against_client_cwd() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-relative")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let caller_dir = harness.tmpdir().join("caller");
    let nested_dir = caller_dir.join("nested");
    fs::create_dir_all(&nested_dir)?;
    fs::write(nested_dir.join("input.txt"), "loaded from relative path")?;

    assert_success(&harness.run_with(
        &["load-buffer", "-b", "loaded", "nested/input.txt"],
        |command| {
            command.current_dir(&caller_dir);
        },
    )?);

    let show = harness.run(&["show-buffer", "-b", "loaded"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "loaded from relative path");
    assert!(stderr(&show).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn run_with_stdin(
    harness: &CliHarness,
    args: &[&str],
    stdin: &[u8],
) -> Result<std::process::Output, Box<dyn Error>> {
    let mut command = harness.base_command();
    command.args(args);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(stdin)?;
    Ok(child.wait_with_output()?)
}

#[test]
fn save_buffer_resolves_relative_paths_against_client_cwd() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer-relative")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let caller_dir = harness.tmpdir().join("caller");
    let nested_dir = caller_dir.join("nested");
    fs::create_dir_all(&nested_dir)?;

    assert_success(&harness.run(&["set-buffer", "-b", "saved", "save this"])?);
    assert_success(&harness.run_with(
        &["save-buffer", "-b", "saved", "nested/output.txt"],
        |command| {
            command.current_dir(&caller_dir);
        },
    )?);

    assert_eq!(
        fs::read_to_string(nested_dir.join("output.txt"))?,
        "save this"
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn save_buffer_replaces_existing_destination_file() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer-replace")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("output.txt");

    std::fs::write(&output_path, "stale data")?;
    assert_success(&harness.run(&["set-buffer", "-b", "saved", "fresh data"])?);
    assert_success(&harness.run(&[
        "save-buffer",
        "-b",
        "saved",
        output_path.to_str().expect("utf-8 test path"),
    ])?);

    assert_eq!(std::fs::read_to_string(&output_path)?, "fresh data");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn save_buffer_preserves_existing_file_identity_metadata_and_links() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer-existing-identity")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("output.txt");
    let hard_link_path = harness.tmpdir().join("output-hard-link.txt");
    let symlink_target = harness.tmpdir().join("symlink-target.txt");
    let symlink_path = harness.tmpdir().join("output-symlink.txt");

    fs::write(&output_path, "stale data")?;
    fs::set_permissions(&output_path, fs::Permissions::from_mode(0o600))?;
    fs::hard_link(&output_path, &hard_link_path)?;
    let original = fs::metadata(&output_path)?;

    assert_success(&harness.run(&["set-buffer", "-b", "saved", "fresh data"])?);
    assert_success(&harness.run(&[
        "save-buffer",
        "-b",
        "saved",
        output_path.to_str().expect("utf-8 test path"),
    ])?);

    let replaced = fs::metadata(&output_path)?;
    assert_eq!(replaced.ino(), original.ino());
    assert_eq!(replaced.mode() & 0o777, 0o600);
    assert_eq!(fs::read_to_string(&hard_link_path)?, "fresh data");

    fs::write(&symlink_target, "target stale data")?;
    fs::set_permissions(&symlink_target, fs::Permissions::from_mode(0o600))?;
    std::os::unix::fs::symlink(&symlink_target, &symlink_path)?;
    let target_inode = fs::metadata(&symlink_target)?.ino();
    assert_success(&harness.run(&[
        "save-buffer",
        "-b",
        "saved",
        symlink_path.to_str().expect("utf-8 test path"),
    ])?);

    assert!(fs::symlink_metadata(&symlink_path)?
        .file_type()
        .is_symlink());
    let updated_target = fs::metadata(&symlink_target)?;
    assert_eq!(updated_target.ino(), target_inode);
    assert_eq!(updated_target.mode() & 0o777, 0o600);
    assert_eq!(fs::read_to_string(&symlink_target)?, "fresh data");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn load_buffer_failure_does_not_replace_existing_buffer() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("load-buffer-failure")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let missing_path = harness.tmpdir().join("missing.txt");

    assert_success(&harness.run(&["set-buffer", "-b", "stable", "original"])?);

    let load = harness.run(&[
        "load-buffer",
        "-b",
        "stable",
        missing_path.to_str().expect("utf-8 test path"),
    ])?;
    assert_eq!(load.status.code(), Some(1));
    assert!(stderr(&load).contains(missing_path.to_str().expect("utf-8 test path")));

    let show = harness.run(&["show-buffer", "-b", "stable"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "original");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn save_buffer_failure_does_not_delete_existing_buffer() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("save-buffer-failure")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("missing-parent").join("output.txt");

    assert_success(&harness.run(&["set-buffer", "-b", "stable", "original"])?);

    let save = harness.run(&[
        "save-buffer",
        "-b",
        "stable",
        output_path.to_str().expect("utf-8 test path"),
    ])?;
    assert_eq!(save.status.code(), Some(1));
    assert!(stderr(&save).contains(output_path.to_str().expect("utf-8 test path")));

    let show = harness.run(&["show-buffer", "-b", "stable"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "original");

    terminate_child(daemon.child_mut())?;
    Ok(())
}
