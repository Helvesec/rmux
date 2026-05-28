#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::time::Duration;

use common::{assert_success, stderr, stdout, CliHarness};

#[test]
fn foreground_run_shell_writes_captured_stdout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-stdout")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "printf hello"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "hello");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn run_shell_nonzero_exits_one_without_stdout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-nonzero")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "printf hidden; exit 9"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(!stderr(&output).is_empty());
    Ok(())
}

#[test]
fn run_shell_preserves_spaced_path_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-spaced-path")?;
    let _daemon = harness.start_hidden_daemon()?;
    let spaced_path = harness.tmpdir().join("name with spaces");

    let output = harness.run(&[
        "run-shell",
        "env",
        "-C",
        harness.tmpdir().to_str().expect("utf-8 test path"),
        "touch",
        "name with spaces",
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(spaced_path.is_file());
    assert!(!harness.tmpdir().join("name").exists());
    assert!(!harness.tmpdir().join("with").exists());
    assert!(!harness.tmpdir().join("spaces").exists());
    Ok(())
}

#[test]
fn run_shell_preserves_shell_metacharacters_and_backslashes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-metacharacters")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "printf", "%s", "x;y\\z"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "x;y\\z");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn if_shell_dispatches_nested_supported_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-dispatch")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "if-shell",
        "-F",
        "1",
        "set-buffer -b selected yes",
        "set-buffer -b selected no",
    ])?);

    let output = harness.run(&["show-buffer", "-b", "selected"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "yes");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn if_shell_preserves_nested_stdout_from_output_commands() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-output")?;
    let _daemon = harness.start_hidden_daemon()?;
    let marker = "if_shell_capture_marker";

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "selected", "yes"])?);

    let display = harness.run(&[
        "if-shell",
        "-F",
        "-t",
        "alpha:0.0",
        "1",
        "display-message -p -t alpha:0.0 #{session_name}",
    ])?;
    assert_eq!(display.status.code(), Some(0));
    assert_eq!(stdout(&display), "alpha\n");
    assert!(stderr(&display).is_empty());

    let show_buffer = harness.run(&["if-shell", "-F", "1", "show-buffer -b selected"])?;
    assert_eq!(show_buffer.status.code(), Some(0));
    assert_eq!(stdout(&show_buffer), "yes");
    assert!(stderr(&show_buffer).is_empty());

    let list_sessions =
        harness.run(&["if-shell", "-F", "1", "list-sessions -F #{session_name}"])?;
    assert_eq!(list_sessions.status.code(), Some(0));
    assert_eq!(stdout(&list_sessions), "alpha\n");
    assert!(stderr(&list_sessions).is_empty());

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        &format!("printf '{marker}\\n'"),
        "Enter",
    ])?);

    let capture = wait_for_if_shell_capture(&harness, marker)?;
    assert!(stdout(&capture).contains(marker));
    assert!(stderr(&capture).is_empty());

    Ok(())
}

#[test]
fn if_shell_nested_run_shell_preserves_spaced_path_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-run-shell-spaced-path")?;
    let _daemon = harness.start_hidden_daemon()?;
    let spaced_path = harness.tmpdir().join("nested name with spaces");
    let nested_command = format!(
        "run-shell env -C {} touch 'nested name with spaces'",
        harness.tmpdir().display()
    );

    let output = harness.run(&["if-shell", "-F", "1", &nested_command])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(spaced_path.is_file());
    assert!(!harness.tmpdir().join("nested").exists());
    assert!(!harness.tmpdir().join("name").exists());
    assert!(!harness.tmpdir().join("with").exists());
    assert!(!harness.tmpdir().join("spaces").exists());
    Ok(())
}

#[test]
fn if_shell_nested_run_shell_preserves_shell_metacharacters_and_backslashes(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-run-shell-metacharacters")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["if-shell", "-F", "1", "run-shell printf %s 'x;y\\z'"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "x;y\\z");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_rejects_non_tmux_switch_client_f_flag() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-switch-client-f")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("unsupported-switch-client.conf");
    fs::write(&config, "switch-client -f read-only\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    let stderr = stderr(&output);
    assert!(stderr.contains("switch-client"));
    assert!(stderr.contains("-f"));
    Ok(())
}

#[test]
fn source_file_missing_path_reports_plain_no_such_file_surface() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-missing")?;
    let _daemon = harness.start_hidden_daemon()?;
    let missing = harness.tmpdir().join("missing.conf");

    let output = harness.run(&["source-file", missing.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        format!("{}: No such file or directory\n", missing.display())
    );
    Ok(())
}

#[test]
fn if_shell_nested_load_buffer_resolves_relative_paths_against_caller_cwd(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-load-buffer-relative")?;
    let _daemon = harness.start_hidden_daemon()?;
    let caller_dir = harness.tmpdir().join("caller");
    let nested_dir = caller_dir.join("nested");
    fs::create_dir_all(&nested_dir)?;
    fs::write(nested_dir.join("input.txt"), "loaded via nested if-shell")?;

    assert_success(&harness.run_with(
        &[
            "if-shell",
            "-F",
            "1",
            "load-buffer -b loaded nested/input.txt",
        ],
        |command| {
            command.current_dir(&caller_dir);
        },
    )?);

    let show = harness.run(&["show-buffer", "-b", "loaded"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "loaded via nested if-shell");
    assert!(stderr(&show).is_empty());
    Ok(())
}

#[test]
fn if_shell_supports_representative_public_commands() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-surface")?;
    let _daemon = harness.start_hidden_daemon()?;
    let buffer_path = harness.tmpdir().join("loaded-buffer.txt");

    fs::write(&buffer_path, "loaded from file")?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha"])?);

    assert_success(&harness.run(&["if-shell", "-F", "1", "set-option -g status off"])?);

    let show_options = harness.run(&["if-shell", "-F", "1", "show-options -g"])?;
    assert_eq!(show_options.status.code(), Some(0));
    assert!(stdout(&show_options).contains("status off"));
    assert!(stderr(&show_options).is_empty());

    let load_buffer_command = format!("load-buffer -b loaded {}", buffer_path.display());
    assert_success(&harness.run(&["if-shell", "-F", "1", &load_buffer_command])?);

    let show_buffer = harness.run(&["show-buffer", "-b", "loaded"])?;
    assert_eq!(show_buffer.status.code(), Some(0));
    assert_eq!(stdout(&show_buffer), "loaded from file");
    assert!(stderr(&show_buffer).is_empty());

    assert_success(&harness.run(&[
        "if-shell",
        "-F",
        "1",
        "select-layout -t alpha:0 even-horizontal",
    ])?);

    let windows = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_layout}"])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(
        stdout(&windows),
        "89f5,80x24,0,0{39x24,0,0,0,40x24,40,0,1}\n"
    );
    assert!(stderr(&windows).is_empty());

    assert_success(&harness.run(&["if-shell", "-F", "1", "select-pane -t alpha:0.1"])?);

    let panes = harness.run(&[
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_index}:#{pane_active}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert!(stdout(&panes).contains("1:1"));
    assert!(stderr(&panes).is_empty());

    Ok(())
}

#[test]
fn hook_surface_smoke_matches_supported_cli_behavior() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-surface-smoke")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-t",
        "al",
        "client-attached",
        "display-message hi",
    ])?);

    let rejected = harness.run(&["set-hook", "-g", "window-resized", "display-message hi"])?;
    assert_eq!(rejected.status.code(), Some(1));
    assert!(stdout(&rejected).is_empty());
    assert_eq!(
        stderr(&rejected),
        "window-resized is not supported: rmux does not dispatch this hook\n"
    );

    let output = harness.run(&["show-hooks", "-t", "al", "client-attached"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "client-attached[0] display-message hi\n");
    assert!(stderr(&output).is_empty());

    let bindings = harness.run(&["list-keys", "-T", "prefix", "C-b"])?;
    assert_eq!(bindings.status.code(), Some(0));
    assert_eq!(stdout(&bindings), "bind-key -T prefix C-b send-prefix\n");
    assert!(stderr(&bindings).is_empty());

    Ok(())
}

#[test]
fn wait_for_signal_succeeds_without_waiters() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("wait-for-signal")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["wait-for", "-S", "no-waiters"])?;

    assert_success(&output);
    Ok(())
}

fn wait_for_if_shell_capture(
    harness: &CliHarness,
    marker: &str,
) -> Result<std::process::Output, Box<dyn Error>> {
    let mut last = None;
    for _ in 0..100 {
        let output = harness.run(&["if-shell", "-F", "1", "capture-pane -p -t alpha:0.0"])?;
        if output.status.code() == Some(0) && stdout(&output).contains(marker) {
            return Ok(output);
        }
        last = Some(output);
        std::thread::sleep(Duration::from_millis(20));
    }

    let last = last.expect("capture was attempted");
    Err(format!(
        "if-shell capture output never contained marker {marker}; status={:?} stdout={:?} stderr={:?}",
        last.status.code(),
        stdout(&last),
        stderr(&last)
    )
    .into())
}
