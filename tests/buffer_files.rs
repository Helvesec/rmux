#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::Write;
use std::process::Stdio;

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
