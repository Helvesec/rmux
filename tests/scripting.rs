#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{assert_success, read_until_contains, stderr, stdout, AttachedSession, CliHarness};
use rmux_proto::TerminalSize;

#[test]
fn foreground_run_shell_prints_stdout_like_tmux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-stdout")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "printf hello"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "hello\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn explicitly_targeted_run_shell_does_not_print_to_caller_like_tmux() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("run-shell-explicit-target-output")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["run-shell", "-t", "alpha:0.0", "printf hidden; exit 7"])?;

    assert_eq!(output.status.code(), Some(7));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_explicitly_targeted_run_shell_does_not_print_to_caller() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("source-run-shell-explicit-target-output")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let config = harness.tmpdir().join("targeted-run-shell.conf");
    fs::write(
        &config,
        "run-shell -t alpha:0.0 'printf source-hidden; exit 6'\n",
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(6));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn explicitly_targeted_run_shell_output_is_delivered_to_the_target_session(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-target-status-delivery")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut attach =
        AttachedSession::spawn(&harness, "alpha", TerminalSize { cols: 80, rows: 12 })?;
    attach.wait_for_raw_mode(Duration::from_secs(5))?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", Duration::from_secs(5))?;

    let direct = harness.run(&[
        "run-shell",
        "-t",
        "alpha:0.0",
        "printf direct-target-visible",
    ])?;
    assert_success(&direct);
    assert!(stdout(&direct).is_empty());
    let _ = read_until_contains(
        attach.master_mut(),
        "direct-target-visible",
        Duration::from_secs(5),
    )?;

    let config = harness.tmpdir().join("target-status-delivery.conf");
    fs::write(
        &config,
        "run-shell -t alpha:0.0 'printf source-target-visible'\n",
    )?;
    let sourced = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;
    assert_success(&sourced);
    assert!(stdout(&sourced).is_empty());
    let _ = read_until_contains(
        attach.master_mut(),
        "source-target-visible",
        Duration::from_secs(5),
    )?;

    attach.send_bytes(b"\x02d")?;
    assert!(attach.wait_for_exit(Duration::from_secs(5))?.success());
    Ok(())
}

#[test]
fn targeted_run_shell_falls_back_to_the_caller_if_the_target_disappears(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-target-disappears")?;
    let _daemon = harness.start_hidden_daemon()?;

    for (session, source_file) in [("direct", false), ("sourced", true)] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        assert_success(&harness.run(&["split-window", "-d", "-t", session])?);
        let target = format!("{session}:0.1");
        let original_pane_id = stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            target.as_str(),
            "#{pane_id}",
        ])?);
        let started = harness.tmpdir().join(format!("{session}-started"));
        let release = harness.tmpdir().join(format!("{session}-release"));
        let shell_command = format!(
            "printf '%s' \"$RMUX_PANE\" > {}; while [ ! -f {} ]; do sleep 0.02; done; printf hello",
            shell_quote(&started),
            shell_quote(&release),
        );
        let config = harness.tmpdir().join(format!("{session}-target-dies.conf"));
        if source_file {
            fs::write(
                &config,
                format!(
                    "run-shell -t {target} {}\n",
                    shell_quote_str(&shell_command)
                ),
            )?;
        }

        let mut command = harness.base_command();
        if source_file {
            command.args(["source-file", config.to_str().expect("utf-8 config path")]);
        } else {
            command.args(["run-shell", "-t", target.as_str(), shell_command.as_str()]);
        }
        let caller = std::thread::spawn(move || command.output());
        if let Err(error) = wait_for_file_text(&started, original_pane_id.trim()) {
            let _ = fs::write(&release, b"");
            return Err(error);
        }
        let killed = run_concurrent_cli(&harness, &["kill-pane", "-t", target.as_str()]);
        fs::write(&release, b"")?;
        assert_success(&killed?);
        let output = caller
            .join()
            .map_err(|_| "run-shell caller thread panicked")??;

        assert_eq!(output.status.code(), Some(0));
        assert_eq!(stdout(&output), "hello\n");
        assert!(stderr(&output).is_empty());
    }
    Ok(())
}

#[test]
fn targeted_run_shell_does_not_deliver_to_a_replacement_in_the_same_pane_slot(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-target-replaced")?;
    let _daemon = harness.start_hidden_daemon()?;

    for (session, source_file) in [("direct-reused", false), ("sourced-reused", true)] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        assert_success(&harness.run(&["split-window", "-d", "-t", session])?);
        let target = format!("{session}:0.1");
        let original_pane_id = stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            target.as_str(),
            "#{pane_id}",
        ])?);
        let started = harness.tmpdir().join(format!("{session}-started"));
        let release = harness.tmpdir().join(format!("{session}-release"));
        let shell_command = format!(
            "printf '%s' \"$RMUX_PANE\" > {}; while [ ! -f {} ]; do sleep 0.02; done; printf replacement-safe",
            shell_quote(&started),
            shell_quote(&release),
        );
        let config = harness
            .tmpdir()
            .join(format!("{session}-target-reused.conf"));
        if source_file {
            fs::write(
                &config,
                format!(
                    "run-shell -t {target} {}\n",
                    shell_quote_str(&shell_command)
                ),
            )?;
        }

        let mut command = harness.base_command();
        if source_file {
            command.args(["source-file", config.to_str().expect("utf-8 config path")]);
        } else {
            command.args(["run-shell", "-t", target.as_str(), shell_command.as_str()]);
        }
        let caller = std::thread::spawn(move || command.output());
        if let Err(error) = wait_for_file_text(&started, original_pane_id.trim()) {
            let _ = fs::write(&release, b"");
            return Err(error);
        }
        let killed = run_concurrent_cli(&harness, &["kill-pane", "-t", target.as_str()]);
        let replacement = harness.run(&["split-window", "-d", "-t", session]);
        let replacement_pane =
            harness.run(&["display-message", "-p", "-t", target.as_str(), "#{pane_id}"]);
        fs::write(&release, b"")?;

        assert_success(&killed?);
        assert_success(&replacement?);
        let replacement_pane_id = stdout(&replacement_pane?);
        assert_ne!(
            replacement_pane_id, original_pane_id,
            "replacement must have a distinct stable pane identity"
        );
        let output = caller
            .join()
            .map_err(|_| "run-shell caller thread panicked")??;

        assert_eq!(output.status.code(), Some(0));
        assert_eq!(stdout(&output), "replacement-safe\n");
        assert!(stderr(&output).is_empty());
    }
    Ok(())
}

#[test]
fn targeted_run_shell_follows_the_stable_pane_after_renumber_or_break() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("run-shell-target-renumbered")?;
    let _daemon = harness.start_hidden_daemon()?;

    for (session, source_file, break_to_window) in [
        ("direct-renumber", false, false),
        ("sourced-renumber", true, false),
        ("direct-broken", false, true),
        ("sourced-broken", true, true),
    ] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        assert_success(&harness.run(&["split-window", "-d", "-t", session])?);
        let original_target = format!("{session}:0.1");
        let original_pane_id = stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            original_target.as_str(),
            "#{pane_id}",
        ])?);
        let mut attach =
            AttachedSession::spawn(&harness, session, TerminalSize { cols: 80, rows: 12 })?;
        attach.wait_for_raw_mode(Duration::from_secs(5))?;
        let _ = read_until_contains(
            attach.master_mut(),
            "tester@RMUXHOST",
            Duration::from_secs(5),
        )?;

        let marker = harness.tmpdir().join(format!("{session}-started"));
        let output_marker = format!("{session}-stable-output");
        let shell_command = format!(
            "printf '%s' \"$RMUX_PANE\" > {}; sleep 1; printf {}",
            shell_quote(&marker),
            shell_quote_str(&output_marker)
        );
        let config = harness
            .tmpdir()
            .join(format!("{session}-target-moved.conf"));
        if source_file {
            fs::write(
                &config,
                format!(
                    "run-shell -t {original_target} {}\n",
                    shell_quote_str(&shell_command)
                ),
            )?;
        }

        let mut command = harness.base_command();
        if source_file {
            command.args(["source-file", config.to_str().expect("utf-8 config path")]);
        } else {
            command.args([
                "run-shell",
                "-t",
                original_target.as_str(),
                shell_command.as_str(),
            ]);
        }
        let caller = std::thread::spawn(move || command.output());
        wait_for_file(&marker)?;
        assert_eq!(fs::read_to_string(&marker)?, original_pane_id.trim());

        let current_target = if break_to_window {
            assert_success(&harness.run(&["break-pane", "-d", "-s", original_target.as_str()])?);
            original_pane_id.trim().to_owned()
        } else {
            assert_success(&harness.run(&["kill-pane", "-t", &format!("{session}:0.0")])?);
            format!("{session}:0.0")
        };
        let moved_pane_id = stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            current_target.as_str(),
            "#{pane_id}",
        ])?);
        assert_eq!(
            moved_pane_id, original_pane_id,
            "surviving pane must keep its stable identity after renumbering"
        );

        let output = caller
            .join()
            .map_err(|_| "run-shell caller thread panicked")??;
        assert_eq!(output.status.code(), Some(0));
        assert!(stdout(&output).is_empty());
        assert!(stderr(&output).is_empty());
        let _ = read_until_contains(attach.master_mut(), &output_marker, Duration::from_secs(5))?;

        attach.send_bytes(b"\x02d")?;
        assert!(attach.wait_for_exit(Duration::from_secs(5))?.success());
    }
    Ok(())
}

#[test]
fn control_mode_targeted_run_shell_follows_the_stable_pane_after_move() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("control-run-shell-target-moved")?;
    let _daemon = harness.start_hidden_daemon()?;

    for (session, break_to_window) in [("control-renumber", false), ("control-break", true)] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        assert_success(&harness.run(&["split-window", "-d", "-t", session])?);
        let original_target = format!("{session}:0.1");
        let marker = harness.tmpdir().join(format!("{session}-started"));
        let completion = harness.tmpdir().join(format!("{session}-completed"));
        let output_marker = format!("{session}-stable-output");
        let shell_command = format!(
            "touch {}; sleep 1; printf {}; touch {}",
            shell_quote(&marker),
            shell_quote_str(&output_marker),
            shell_quote(&completion)
        );
        let mut attach =
            AttachedSession::spawn(&harness, session, TerminalSize { cols: 80, rows: 12 })?;
        attach.wait_for_raw_mode(Duration::from_secs(5))?;
        let _ = read_until_contains(
            attach.master_mut(),
            "tester@RMUXHOST",
            Duration::from_secs(5),
        )?;

        let mut command = harness.base_command();
        let mut control = command
            .arg("-C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        writeln!(
            control.stdin.as_mut().expect("control stdin"),
            "run-shell -t {original_target} {}",
            shell_quote_str(&shell_command)
        )?;
        wait_for_file(&marker)?;

        if break_to_window {
            assert_success(&harness.run(&["break-pane", "-d", "-s", original_target.as_str()])?);
        } else {
            assert_success(&harness.run(&["kill-pane", "-t", &format!("{session}:0.0")])?);
        }
        wait_for_file(&completion)?;

        drop(control.stdin.take());
        let output = control.wait_with_output()?;
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
        assert!(
            !stdout(&output).contains(&output_marker),
            "targeted output must not fall back into control-mode stdout"
        );
        let _ = read_until_contains(attach.master_mut(), &output_marker, Duration::from_secs(5))?;

        attach.send_bytes(b"\x02d")?;
        assert!(attach.wait_for_exit(Duration::from_secs(5))?.success());
    }
    Ok(())
}

#[test]
fn run_shell_exports_tmux_env_matching_mux_socket() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-tmux-env")?;
    let _daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("mux-env.txt");
    let command = format!(
        "printf '%s\n%s\n' \"$TMUX\" \"$RMUX\" > {}",
        shell_quote(&output_path)
    );

    let output = harness.run(&["run-shell", &command])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    let rendered = fs::read_to_string(&output_path)?;
    let lines = rendered.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        2,
        "unexpected run-shell env output: {rendered:?}"
    );
    assert_eq!(lines[0], lines[1]);
    let parts = lines[0].split(',').collect::<Vec<_>>();
    assert_eq!(
        parts.len(),
        3,
        "TMUX must be <socket>,<pid>,<id>: {}",
        lines[0]
    );
    assert!(!parts[0].is_empty(), "TMUX socket path must be present");
    assert!(parts[1].parse::<u32>().is_ok(), "TMUX pid must be numeric");
    assert_eq!(parts[2], "0");
    Ok(())
}

#[test]
fn run_shell_exports_parseable_tmux_program_shim() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-tmux-program")?;
    let _daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("tmux-version.txt");
    let command = format!("\"$TMUX_PROGRAM\" -V > {}", shell_quote(&output_path));

    let output = harness.run(&["run-shell", &command])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    let rendered = fs::read_to_string(output_path)?;
    assert!(
        rendered.starts_with("tmux "),
        "unexpected tmux shim version output: {rendered:?}"
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn run_shell_nonzero_preserves_exit_status_and_stdout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-nonzero")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "printf hidden; exit 9"])?;

    assert_eq!(output.status.code(), Some(9));
    assert_eq!(
        stdout(&output),
        "hidden\n'printf hidden; exit 9' returned 9\n"
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_run_shell_nonzero_preserves_exit_status_and_stdout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-run-shell-nonzero")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("run-shell-nonzero.conf");
    fs::write(&config, "run-shell 'printf hidden; exit 7'\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        stdout(&output),
        "hidden\n'printf hidden; exit 7' returned 7\n"
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_later_successful_run_shell_clears_prior_run_shell_status(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-run-shell-clears-status")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("run-shell-clears-status.conf");
    fs::write(&config, "run-shell 'exit 7'\nrun-shell 'exit 0'\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "'exit 7' returned 7\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_commands_follow_implicit_selected_window_context() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-implicit-window-context")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "a"])?);
    let config = harness.tmpdir().join("implicit-window-context.conf");
    fs::write(
        &config,
        "new-window -d -t a:1 -n w1\nselect-window -t a:1\nmove-window -t a:3\n",
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_success(&output);
    let windows = harness.run(&[
        "list-windows",
        "-t",
        "a",
        "-F",
        "#{window_index}:#{window_active}:#{window_name}",
    ])?;
    let listed = stdout(&windows);
    let lines = listed.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].starts_with("0:0:"),
        "initial shell window should remain inactive: {listed:?}"
    );
    assert_eq!(lines[1], "3:1:w1");
    assert!(stderr(&windows).is_empty());
    Ok(())
}

#[test]
fn source_file_select_layout_supports_navigation_noop_and_target_validation(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-select-layout-modes")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-d", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "even-horizontal"])?);

    let baseline =
        stdout(&harness.run(&["display-message", "-p", "-t", "alpha:0", "#{window_layout}"])?);
    let navigation = harness.tmpdir().join("select-layout-navigation.conf");
    fs::write(
        &navigation,
        "select-layout -n -t alpha:0\n\
         display-message -p -t alpha:0 '#{window_layout}'\n\
         select-layout -p -t alpha:0\n\
         display-message -p -t alpha:0 '#{window_layout}'\n",
    )?;

    assert_success(&harness.run(&[
        "source-file",
        "-n",
        navigation.to_str().expect("utf-8 config path"),
    ])?);

    let output = harness.run(&[
        "source-file",
        navigation.to_str().expect("utf-8 config path"),
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    let layouts = stdout(&output)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        layouts.len(),
        2,
        "unexpected source-file output: {output:?}"
    );
    assert_ne!(
        layouts[0],
        baseline.trim_end(),
        "-n must advance the layout"
    );
    assert_eq!(
        layouts[1],
        baseline.trim_end(),
        "-p must return to the prior layout"
    );

    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "after-select-layout",
        "set-buffer -b select-layout-noop-hook fired",
    ])?);
    let noop = harness.tmpdir().join("select-layout-noop.conf");
    fs::write(
        &noop,
        "select-layout -t alpha:0\n\
         display-message -p -t alpha:0 '#{window_layout}'\n",
    )?;
    let output = harness.run(&["source-file", noop.to_str().expect("utf-8 config path")])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), baseline);
    assert!(stderr(&output).is_empty());
    let hook_marker = harness.run(&["show-buffer", "-b", "select-layout-noop-hook"])?;
    assert_eq!(
        hook_marker.status.code(),
        Some(1),
        "a no-op must preserve the direct CLI hook behavior"
    );

    let missing = harness.tmpdir().join("select-layout-missing-target.conf");
    fs::write(&missing, "select-layout -t missing:0\n")?;
    let output = harness.run(&["source-file", missing.to_str().expect("utf-8 config path")])?;
    assert_eq!(output.status.code(), Some(1));
    let rendered = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        rendered.contains("can't find session: missing"),
        "a no-op must still validate its target: {rendered:?}"
    );
    Ok(())
}

#[test]
fn run_shell_dash_e_merges_stderr_like_tmux37() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-stderr")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "-E", "printf err >&2"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "err\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_shell_signal_termination_uses_tmux_message() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-signal-message")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "kill -9 $$"])?;

    assert_eq!(output.status.code(), Some(137));
    assert_eq!(stdout(&output), "'kill -9 $$' terminated by signal 9\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn run_shell_stdout_is_drained_without_cli_output() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-stdout-cap")?;
    let _daemon = harness.start_hidden_daemon()?;
    let command = "dd if=/dev/zero bs=1049600 count=1 2>/dev/null | tr '\\000' A";

    let output = harness.run(&["run-shell", command])?;

    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout(&output);
    assert!(
        stdout.len() > 900_000,
        "run-shell stdout should drain a large payload, got {} bytes",
        stdout.len()
    );
    assert!(stdout.starts_with("AAAA"));
    assert!(stdout.ends_with("\nrmux: run-shell output truncated\n"));
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn run_shell_preserves_spaced_path_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-spaced-path")?;
    let _daemon = harness.start_hidden_daemon()?;
    let spaced_path = harness.tmpdir().join("name with spaces");

    let output = harness.run(&[
        "run-shell",
        "touch \"#{1}\"",
        spaced_path.to_str().expect("utf-8 test path"),
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
    let output_path = harness.tmpdir().join("metacharacters.txt");
    let command = format!("printf '%s' 'x;y\\z' > {}", shell_quote(&output_path));

    let output = harness.run(&["run-shell", &command])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    assert_eq!(fs::read_to_string(output_path)?, "x;y\\z");
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
fn if_shell_resolves_runtime_target_selectors_before_dispatch() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-runtime-targets")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alphabet"])?);

    let ids = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alphabet:0.0",
        "#{session_id}:#{window_id}:#{pane_id}",
    ])?;
    assert_eq!(ids.status.code(), Some(0));
    assert!(stderr(&ids).is_empty());
    let ids = stdout(&ids);
    let ids = ids.trim().split(':').collect::<Vec<_>>();
    assert_eq!(ids.len(), 3, "expected session, window, and pane ids");

    for target in ["alph", "alpha*", "=alphabet:", ids[0], ids[1], ids[2]] {
        assert_success(&harness.run(&[
            "if-shell",
            "-F",
            "-t",
            target,
            "#{==:#{session_name},alphabet}",
            "set-buffer -b if-shell-target resolved",
            "set-buffer -b if-shell-target wrong-context",
        ])?);
        let selected = harness.run(&["show-buffer", "-b", "if-shell-target"])?;
        assert_eq!(selected.status.code(), Some(0));
        assert!(stderr(&selected).is_empty());
        assert_eq!(
            stdout(&selected),
            "resolved",
            "if-shell target {target:?} used the wrong format context"
        );
    }

    Ok(())
}

#[test]
fn if_shell_missing_target_keeps_the_server_fallback() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-missing-target-fallback")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&[
        "if-shell",
        "-F",
        "-t",
        "missing",
        "1",
        "display-message -p #{session_name}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "alpha\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn if_shell_target_resolution_has_one_logical_command_error_boundary() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("if-shell-command-error-boundary")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "if-shell-hook", "seed"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "command-error",
        "set-buffer -a -b if-shell-hook x",
    ])?);

    let missing = harness.run(&[
        "if-shell",
        "-F",
        "-t",
        "missing",
        "1",
        "set-buffer -b if-shell-selected yes",
    ])?;
    assert_success(&missing);
    assert!(stderr(&missing).is_empty());
    assert_eq!(
        stdout(&harness.run(&["show-buffer", "-b", "if-shell-hook"])?),
        "seed",
        "a successful missing-target fallback must not emit command-error"
    );

    let invalid = harness.run(&[
        "if-shell",
        "-F",
        "-t",
        "$bogus",
        "1",
        "set-buffer -b if-shell-selected no",
    ])?;
    assert_eq!(invalid.status.code(), Some(1));
    assert!(stderr(&invalid).contains("must be followed by an unsigned integer"));
    assert_eq!(
        stdout(&harness.run(&["show-buffer", "-b", "if-shell-hook"])?),
        "seedx",
        "one failed logical if-shell command must emit command-error exactly once"
    );
    Ok(())
}

#[test]
fn if_shell_current_target_can_fail_on_an_empty_server() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-empty-current-target")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-g", "exit-empty", "off"])?);
    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);

    let output = harness.run(&[
        "if-shell",
        "-F",
        "-t",
        ".",
        "1",
        "set-buffer -b if-shell-empty-current selected",
    ])?;
    assert_success(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    assert_eq!(
        stdout(&harness.run(&["show-buffer", "-b", "if-shell-empty-current",])?),
        "selected"
    );
    Ok(())
}

#[test]
fn if_shell_ignores_a_stale_inherited_pane_without_emitting_command_error(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-stale-inherited-pane")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "if-shell-hook", "seed"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "command-error",
        "set-buffer -a -b if-shell-hook x",
    ])?);

    let rmux = format!("{},0,0", harness.socket_path().display());
    let output = harness.run_with(
        &[
            "if-shell",
            "-F",
            "-t",
            "alpha",
            "1",
            "set-buffer -b if-shell-selected yes",
        ],
        |command| {
            command.env("RMUX", rmux);
            command.env("RMUX_PANE", "%999999");
        },
    )?;
    assert_success(&output);
    assert!(stderr(&output).is_empty());
    assert_eq!(
        stdout(&harness.run(&["show-buffer", "-b", "if-shell-hook"])?),
        "seed",
        "a stale inherited pane must not create an auxiliary command-error boundary"
    );
    assert_eq!(
        stdout(&harness.run(&["show-buffer", "-b", "if-shell-selected"])?),
        "yes"
    );
    Ok(())
}

#[test]
fn if_shell_preserves_source_file_shaped_stdout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-source-shaped-stdout")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["if-shell", "-F", "1", "display-message -p -- '-:1: hello'"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "-:1: hello\n");
    assert!(stderr(&output).is_empty());

    let invalid = harness.run(&[
        "if-shell",
        "-F",
        "-t",
        "$bogus",
        "1",
        "display-message -p unreachable",
    ])?;
    assert_eq!(invalid.status.code(), Some(1));
    assert!(stdout(&invalid).is_empty());
    assert!(stderr(&invalid).contains("must be followed by an unsigned integer"));
    Ok(())
}

#[test]
fn if_shell_propagates_a_nested_source_failure_after_stdout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-nested-source-status")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let invalid_config = harness.tmpdir().join("invalid.conf");
    fs::write(&invalid_config, "definitely-not-an-rmux-command\n")?;
    let branch = format!(
        "display-message -p ok ; source-file '{}'",
        invalid_config.display()
    );

    let output = harness.run(&["if-shell", "-F", "1", &branch])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).starts_with("ok\n"));
    assert!(stdout(&output).contains("unknown command"));
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn if_shell_mouse_target_keeps_the_server_fallback() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-mouse-target-fallback")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&[
        "if-shell",
        "-F",
        "-t",
        "{mouse}",
        "1",
        "display-message -p #{session_name}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "alpha\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn nested_commands_report_missing_session_without_invalid_target_wrapper(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("nested-missing-session-error")?;
    let _daemon = harness.start_hidden_daemon()?;

    let run_shell = harness.run(&["run-shell", "-C", "has-session -t missing"])?;
    assert_eq!(run_shell.status.code(), Some(1));
    assert!(stdout(&run_shell).is_empty());
    assert_eq!(stderr(&run_shell), "can't find session: missing\n");

    let if_shell = harness.run(&["if-shell", "-F", "1", "has-session -t missing"])?;
    assert_eq!(if_shell.status.code(), Some(1));
    assert!(stdout(&if_shell).is_empty());
    assert_eq!(stderr(&if_shell), "can't find session: missing\n");
    Ok(())
}

#[test]
fn run_shell_command_mode_ignores_trailing_positional_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-c-ignore-positionals")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "run-shell",
        "-C",
        "set-buffer -b positional #{1}-#{2}",
        "alpha",
        "beta",
    ])?);

    let output = harness.run(&["show-buffer", "-b", "positional"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "-");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn run_shell_command_mode_attach_session_requires_terminal() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-c-attach-requires-terminal")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["run-shell", "-C", "attach-session -t alpha"])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "open terminal failed: not a terminal\n");
    Ok(())
}

#[test]
fn run_shell_empty_command_forms_are_noops() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-empty-noops")?;
    let _daemon = harness.start_hidden_daemon()?;

    for args in [
        vec!["run-shell"],
        vec!["run-shell", "-b"],
        vec!["run-shell", "-C"],
    ] {
        let output = harness.run(&args)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(stdout(&output).is_empty(), "{args:?}");
        assert!(stderr(&output).is_empty(), "{args:?}");
    }
    Ok(())
}

#[test]
fn run_shell_commands_use_explicit_target_for_implicit_nested_target() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("run-shell-c-explicit-target")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);

    assert_success(&harness.run(&["run-shell", "-t", "alpha:0.0", "-C", "split-window -h"])?);

    let panes = harness.run(&["list-panes", "-a", "-F", "#{session_name}:#{pane_index}"])?;
    assert_eq!(panes.status.code(), Some(0), "stderr={}", stderr(&panes));
    assert!(stderr(&panes).is_empty());
    let panes = stdout(&panes);
    assert!(
        panes.lines().any(|line| line == "alpha:1"),
        "run-shell -C -t should use the explicit target, got:\n{panes}"
    );
    assert!(
        !panes.lines().any(|line| line == "beta:1"),
        "run-shell -C -t should not ignore the explicit target, got:\n{panes}"
    );
    Ok(())
}

#[test]
fn source_file_list_panes_all_preserves_filter_sort_and_reverse() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-list-panes-all-options")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "beta"])?);

    let config = harness.tmpdir().join("list-panes-all.conf");
    fs::write(
        &config,
        "list-panes -ar -O index -f '#{||:#{==:#{pane_index},0},#{==:#{pane_index},1}}' -F '#{session_name}:#{pane_index}'\n",
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={:?}, stderr={:?}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "alpha:1\nalpha:0\nbeta:1\nbeta:0\n");
    assert!(stderr(&output).is_empty());

    Ok(())
}

#[test]
fn prompts_without_attached_client_report_no_current_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("prompt-no-current-client")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let command_prompt = harness.run(&["command-prompt", "-p", "prompt", "display-message %1"])?;
    assert_eq!(command_prompt.status.code(), Some(1));
    assert!(stdout(&command_prompt).is_empty());
    assert_eq!(stderr(&command_prompt), "no current client\n");

    let confirm_before = harness.run(&["confirm-before", "-p", "sure", "display-message ok"])?;
    assert_eq!(confirm_before.status.code(), Some(1));
    assert!(stdout(&confirm_before).is_empty());
    assert_eq!(stderr(&confirm_before), "no current client\n");
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
fn if_shell_preserves_prior_stdout_when_a_later_nested_command_fails() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("if-shell-output-before-error")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&[
        "if-shell",
        "-F",
        "1",
        "display-message -p BEFORE ; select-window -t definitely-missing",
    ])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "BEFORE\n");
    assert_eq!(stderr(&output), "can't find window: definitely-missing\n");
    Ok(())
}

#[test]
fn if_shell_format_truthiness_matches_tmux_numeric_zero_prefix() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-format-truthiness")?;
    let _daemon = harness.start_hidden_daemon()?;

    let leading_zero = harness.run(&[
        "if-shell",
        "-F",
        "09",
        "display-message -p TRUE",
        "display-message -p FALSE",
    ])?;
    assert_eq!(leading_zero.status.code(), Some(0));
    assert_eq!(stdout(&leading_zero), "FALSE\n");

    let repeated_zero = harness.run(&[
        "if-shell",
        "-F",
        "00",
        "display-message -p TRUE",
        "display-message -p FALSE",
    ])?;
    assert_eq!(repeated_zero.status.code(), Some(0));
    assert_eq!(stdout(&repeated_zero), "FALSE\n");

    let exact_zero = harness.run(&[
        "if-shell",
        "-F",
        "0",
        "display-message -p TRUE",
        "display-message -p FALSE",
    ])?;
    assert_eq!(exact_zero.status.code(), Some(0));
    assert_eq!(stdout(&exact_zero), "FALSE\n");
    Ok(())
}

#[test]
fn if_shell_nested_run_shell_preserves_spaced_path_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("if-shell-run-shell-spaced-path")?;
    let _daemon = harness.start_hidden_daemon()?;
    let spaced_path = harness.tmpdir().join("nested name with spaces");
    let nested_command = format!(
        "run-shell {} {}",
        shell_quote_str("touch \"#{1}\""),
        shell_quote(&spaced_path)
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
    let output_path = harness.tmpdir().join("nested-metacharacters.txt");
    let shell_command = format!("printf '%s' 'x;y\\z' > {}", shell_quote(&output_path));
    let nested_command = format!("run-shell {}", shell_quote_str(&shell_command));

    let output = harness.run(&["if-shell", "-F", "1", &nested_command])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    assert_eq!(fs::read_to_string(output_path)?, "x;y\\z");
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
    assert_eq!(
        stdout(&output),
        format!(
            "{}:1: command switch-client: unknown flag -f\n",
            config.display()
        )
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_reports_invalid_command_flags_on_stdout_with_line_prefix(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-invalid-command-flags")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("unsupported-command-flags.conf");
    fs::write(
        &config,
        "capture-pane -Z\nchoose-tree -y 1\nlist-clients -Q\nlist-buffers -Q\n",
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stdout(&output),
        format!(
            "{}:1: command capture-pane: unknown flag -Z\n{}:2: command choose-tree: unknown flag -y\n{}:3: command list-clients: unknown flag -Q\n{}:4: command list-buffers: unknown flag -Q\n",
            config.display(),
            config.display(),
            config.display(),
            config.display()
        )
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_execution_errors_report_on_stderr_without_line_prefix() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("source-file-execution-error-stderr")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("execution-error.conf");
    fs::write(&config, "has-session -t missing\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "can't find session: missing\n");
    Ok(())
}

#[test]
fn source_file_break_between_group_aliases_with_multiple_panes_succeeds(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-grouped-break-multiple-panes")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "owner"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "owner:0"])?);
    assert_success(&harness.run(&["new-session", "-d", "-t", "owner", "-s", "peer"])?);
    let moved_pane_id =
        stdout(&harness.run(&["display-message", "-p", "-t", "owner:0.1", "#{pane_id}"])?);
    let config = harness.tmpdir().join("grouped-break.conf");
    fs::write(&config, "break-pane -d -s owner:0.1 -t peer:1\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_success(&output);
    for target in ["owner:1.0", "peer:1.0"] {
        assert_eq!(
            stdout(&harness.run(&["display-message", "-p", "-t", target, "#{pane_id}",])?),
            moved_pane_id,
        );
    }
    let source_panes =
        stdout(&harness.run(&["list-panes", "-t", "owner:0", "-F", "#{pane_id}"])?);
    assert_eq!(source_panes.lines().count(), 1);
    assert_ne!(source_panes, moved_pane_id);
    Ok(())
}

#[test]
fn source_file_break_last_pane_between_group_aliases_matches_tmux_rejection_without_mutation(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-grouped-break-last-pane-rejection")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "owner"])?);
    assert_success(&harness.run(&["new-session", "-d", "-t", "owner", "-s", "peer"])?);
    let before = stdout(&harness.run(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}:#{window_index}.#{pane_index}:#{pane_id}",
    ])?);
    let config = harness.tmpdir().join("grouped-break-last-pane.conf");
    fs::write(&config, "break-pane -d -s owner:0.0 -t peer:1\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "sessions are grouped\n");
    let after = stdout(&harness.run(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}:#{window_index}.#{pane_index}:#{pane_id}",
    ])?);
    assert_eq!(after, before);
    Ok(())
}

#[test]
fn source_file_parse_errors_report_input_line_number() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-line-number")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("bad-line.conf");
    fs::write(
        &config,
        "set-option -g @before ok\nbogus-command\nset-option -g @after ok\n",
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stdout(&output),
        format!("{}:2: unknown command: bogus-command\n", config.display())
    );
    assert!(stderr(&output).is_empty());
    for option in ["@before", "@after"] {
        let output = harness.run(&["show-options", "-gqv", option])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "show-options -q should stay quiet for {option}"
        );
        assert!(stdout(&output).is_empty(), "{option} should not be set");
        assert!(stderr(&output).is_empty());
    }
    Ok(())
}

#[test]
fn source_file_config_parse_errors_prevent_neighbor_command_execution() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("source-file-error-status")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("error-status.conf");
    fs::write(
        &config,
        "run-shell 'exit 7'\ndefinitely-not-a-command\ndisplay-message -p after\n",
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stdout(&output),
        format!(
            "{}:2: unknown command: definitely-not-a-command\n",
            config.display()
        )
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_config_errors_are_logged_once() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-error-log-once")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("error-log-once.conf");
    fs::write(&config, "definitely-not-a-command\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;
    assert_eq!(output.status.code(), Some(1));

    let messages = harness.run(&["show-messages"])?;
    assert_eq!(messages.status.code(), Some(0));
    let rendered = stdout(&messages);
    let needle = format!(
        "config error: {}:1: unknown command: definitely-not-a-command",
        config.display()
    );
    assert_eq!(
        rendered.matches(&needle).count(),
        1,
        "source-file config error should be logged exactly once, got {rendered:?}"
    );
    assert!(stderr(&messages).is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn source_file_parse_only_does_not_run_plugin_shell_commands() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-parse-only-cli")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("parse-only.conf");
    let marker = harness.tmpdir().join("must-not-run");
    fs::write(
        &config,
        format!(
            "set -g @parse-only-probe yes\nrun-shell 'touch {}'\n",
            shell_quote(&marker)
        ),
    )?;

    let output = harness.run(&[
        "source-file",
        "-n",
        "-v",
        config.to_str().expect("utf-8 config path"),
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout(&output).contains("set-option -g @parse-only-probe yes"),
        "parse-only should report parsed commands, got {:?}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("run-shell"),
        "parse-only should report plugin run-shell commands, got {:?}",
        stdout(&output)
    );
    assert!(stderr(&output).is_empty());
    assert!(
        !marker.exists(),
        "source-file -n must parse config without running plugin shell commands"
    );
    Ok(())
}

#[test]
fn source_file_parse_only_validates_command_flags_like_tmux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-parse-only-invalid-cli")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("bad.conf");
    fs::write(&config, "new-window -Q\n")?;

    let output = harness.run(&[
        "source-file",
        "-n",
        config.to_str().expect("utf-8 config path"),
    ])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).contains("command new-window: unknown flag -Q"),
        "stdout={:?}",
        stdout(&output)
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_parse_only_accepts_implicit_target_commands_like_tmux() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("source-file-parse-only-implicit-target")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "parseonly"])?);
    let config = harness.tmpdir().join("parse-only-implicit-target.conf");
    fs::write(&config, "clear-history\nset -g @after-parse-only yes\n")?;

    let output = harness.run(&[
        "source-file",
        "-n",
        "-v",
        config.to_str().expect("utf-8 config path"),
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        format!(
            "{}:1: clear-history\n{}:2: set-option -g @after-parse-only yes\n",
            config.display(),
            config.display()
        )
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_clear_history_without_target_uses_implicit_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-clear-history-implicit")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "clearhist"])?);
    let config = harness.tmpdir().join("clear-history-implicit.conf");
    fs::write(&config, "clear-history\nset -g @after-clear-history yes\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_success(&output);
    let option = harness.run(&["show-options", "-gqv", "@after-clear-history"])?;
    assert_eq!(option.status.code(), Some(0));
    assert_eq!(stdout(&option), "yes\n");
    assert!(stderr(&option).is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn source_file_recursion_through_tmux_shim_is_bounded() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-shim-recursion")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("self.conf");
    fs::write(
        &config,
        format!("run-shell 'tmux source-file {}'\n", shell_quote(&config)),
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    let stdout = stdout(&output);
    assert!(stdout.contains("'tmux source-file "));
    assert!(stdout.contains("' returned 1\n"));
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn startup_config_parse_errors_skip_bad_file_without_aborting_startup() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("startup-config-nonfatal")?;
    let config = harness.tmpdir().join("bad-startup.conf");
    fs::write(
        &config,
        "definitely-not-a-command\nset-option -g @after-startup yes\n",
    )?;

    let output = harness.run(&[
        "-f",
        config.to_str().expect("utf-8 config path"),
        "new-session",
        "-d",
        "-s",
        "boot",
    ])?;

    assert_success(&output);
    assert_success(&harness.run(&["has-session", "-t", "boot"])?);

    let option = harness.run(&["show-options", "-gqv", "@after-startup"])?;
    assert_eq!(option.status.code(), Some(0));
    assert!(stdout(&option).is_empty());
    assert!(stderr(&option).is_empty());
    Ok(())
}

#[test]
fn startup_config_run_shell_can_call_back_into_daemon() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("startup-config-reentrant")?;
    let config = harness.tmpdir().join("reentrant.conf");
    let marker = harness.tmpdir().join("startup-ok.txt");
    let command = format!(
        "{} -S {} show-options -g > {}",
        shell_quote_str(env!("CARGO_BIN_EXE_rmux")),
        shell_quote(harness.socket_path()),
        shell_quote(&marker)
    );
    fs::write(
        &config,
        format!("run-shell {}\n", shell_quote_str(&command)),
    )?;

    let output = harness.run(&[
        "-f",
        config.to_str().expect("utf-8 config path"),
        "new-session",
        "-d",
        "-s",
        "boot",
    ])?;

    assert_success(&output);
    wait_for_file(&marker)?;
    let callback_output = fs::read_to_string(&marker)?;
    assert!(
        callback_output.contains("base-index"),
        "startup source callback must execute against the still-loading daemon: {callback_output:?}"
    );
    Ok(())
}

#[test]
fn startup_readiness_waits_for_the_complete_slow_source_queue() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("startup-readiness-complete-source")?;
    let config = harness.tmpdir().join("slow-startup.conf");
    fs::write(
        &config,
        "run-shell 'sleep 2.25'\nset-option -g base-index 7\n",
    )?;

    let output = harness.run(&[
        "-f",
        config.to_str().expect("utf-8 config path"),
        "new-session",
        "-d",
        "-s",
        "slow",
    ])?;

    assert_success(&output);
    let windows = harness.run(&["list-windows", "-t", "slow", "-F", "#{window_index}"])?;
    assert_eq!(windows.status.code(), Some(0));
    assert_eq!(stdout(&windows), "7\n");
    assert!(stderr(&windows).is_empty());
    Ok(())
}

#[test]
fn source_queue_routes_inventory_start_server_and_new_window_flags() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-queue-inventory-new-window")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "solo", "-n", "old"])?);
    let solo_before = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "solo:0",
        "#{window_id}|#{pane_id}",
    ])?;
    assert_eq!(solo_before.status.code(), Some(0));
    assert!(stderr(&solo_before).is_empty());

    let config = harness.tmpdir().join("queued-command-surface.conf");
    fs::write(
        &config,
        concat!(
            "new-window -dF 'IGNORED=#{window_index}' -t alpha:1 -n old\n",
            "new-window -dkP -F 'K=#{window_index}|#{window_name}' -t alpha:1 -n replacement\n",
            "new-window -d -t alpha:2 -n reuse\n",
            "select-window -t alpha:0\n",
            "new-window -SP -F 'SHOULD-NOT-PRINT' -t alpha: -n reuse\n",
            "new-window -dkP -F 'SOLO=#{window_index}|#{window_id}|#{pane_id}|#{window_name}' -t solo:0 -n replacement\n",
            "start-server\n",
            "list-commands -F '#{command_list_name}|#{command_list_usage}' send-keys\n",
            "list-commands -F '#{command_list_name}|#{command_list_usage}' set-buffer\n",
            "list-commands -F '#{command_list_name}|#{command_list_usage}' set-environment\n",
            "list-commands -F '#{command_list_name}|#{command_list_usage}' show-environment\n",
            "list-commands -F '#{command_list_name}|#{command_list_usage}' show-hooks\n",
            "list-commands -F '#{command_list_name}|#{command_list_usage}' switch-client\n",
            "display-message -p AFTER\n",
        ),
    )?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty(), "stderr={:?}", stderr(&output));
    let lines = stdout(&output)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "K=1|replacement");
    let solo_fields = lines[1].split('|').collect::<Vec<_>>();
    assert_eq!(solo_fields.len(), 4, "solo output={:?}", lines[1]);
    assert_eq!(solo_fields[0], "SOLO=0");
    assert_eq!(solo_fields[3], "replacement");
    let previous_ids = stdout(&solo_before);
    assert!(
        !previous_ids.contains(solo_fields[1]) && !previous_ids.contains(solo_fields[2]),
        "queued -k must allocate fresh window and pane ids: before={previous_ids:?}, after={:?}",
        lines[1]
    );
    assert_eq!(
        &lines[2..],
        &[
            "send-keys|[-FHKlMRX] [-c target-client] [-N repeat-count] [-t target-pane] [key ...]",
            "set-buffer|[-aw] [-b buffer-name] [-n new-buffer-name] [-t target-client] [data]",
            "set-environment|[-Fhgru] [-t target-session] variable [value]",
            "show-environment|[-hgs] [-t target-session] [variable]",
            "show-hooks|[-gpw] [-t target-pane] [hook]",
            "switch-client|[-ElnprZ] [-c target-client] [-t target-session] [-T key-table] [-O order]",
            "AFTER",
        ]
    );

    let active = harness.run(&["display-message", "-p", "-t", "alpha", "#{window_index}"])?;
    assert_eq!(stdout(&active), "2\n");
    Ok(())
}

#[test]
fn nested_command_list_routes_inventory_start_server_and_new_window_printing(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("nested-command-list-inventory-new-window")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);

    let output = harness.run(&[
        "run-shell",
        "-C",
        "start-server ; new-window -dP -F 'NESTED=##{window_index}|##{window_name}' -t alpha:4 -n nested ; new-window -dP -t alpha:5 -n default-print ; list-commands -F '##{command_list_name}|##{command_list_usage}' show-hooks",
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        "NESTED=4|nested\nalpha:5.0\nshow-hooks|[-gpw] [-t target-pane] [hook]\n"
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn startup_queue_routes_inventory_start_server_and_new_window_flags() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("startup-queue-inventory-new-window")?;
    let config = harness.tmpdir().join("startup-command-surface.conf");
    fs::write(
        &config,
        concat!(
            "start-server\n",
            "list-commands send-keys\n",
            "list-commands set-buffer\n",
            "list-commands set-environment\n",
            "list-commands show-environment\n",
            "list-commands show-hooks\n",
            "list-commands switch-client\n",
            "new-session -d -s configured -n zero\n",
            "new-window -dF 'IGNORED' -t configured:1 -n old\n",
            "new-window -dkP -F 'REPLACED=#{window_index}' -t configured:1 -n replacement\n",
            "new-window -d -t configured:2 -n reuse\n",
            "select-window -t configured:0\n",
            "new-window -SP -F 'SHOULD-NOT-PRINT' -t configured: -n reuse\n",
            "set-option -g @startup-queue-complete yes\n",
        ),
    )?;

    let output = harness.run(&[
        "-f",
        config.to_str().expect("utf-8 config path"),
        "new-session",
        "-d",
        "-s",
        "outer",
    ])?;
    assert_success(&output);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "configured",
        "-F",
        "#{window_index}|#{window_name}|#{window_active}",
    ])?;
    assert_eq!(stdout(&windows), "0|zero|0\n1|replacement|0\n2|reuse|1\n");
    let complete = harness.run(&["show-options", "-gqv", "@startup-queue-complete"])?;
    assert_eq!(stdout(&complete), "yes\n");
    let messages = harness.run(&["show-messages"])?;
    assert!(
        !stdout(&messages).contains("config error:"),
        "startup queue commands must not record config errors: {:?}",
        stdout(&messages)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn tmux_fallback_nested_shim_missing_local_is_not_logged_as_config_error(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("tmux-fallback-missing-local-message")?;
    let home = harness.tmpdir().join("home");
    let fake_bin = harness.tmpdir().join("fake-bin");
    fs::create_dir_all(&fake_bin)?;
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rmux"), fake_bin.join("tmux"))?;
    fs::write(
        home.join(".tmux.conf"),
        "run-shell 'tmux -S #{socket_path} source \"$HOME/.tmux.conf.local\"'\n\
         set -g @after-missing-local yes\n",
    )?;

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    assert_success(
        &harness.run_with(&["new-session", "-d", "-s", "missinglocal"], |command| {
            command.env_remove("RMUX_DISABLE_TMUX_FALLBACK");
            command.env("PATH", &path);
        })?,
    );

    wait_for_option_value(&harness, "@after-missing-local", "yes")?;
    let messages = harness.run(&["show-messages"])?;
    assert_eq!(messages.status.code(), Some(0));
    let rendered = stdout(&messages);
    assert!(
        rendered.contains(".tmux.conf.local") && rendered.contains("No such file"),
        "missing optional tmux local file should remain visible as a client-style message, got {rendered:?}"
    );
    assert!(
        !rendered.contains("config error:"),
        "missing optional tmux local file should not be logged as a config error, got {rendered:?}"
    );
    assert!(stderr(&messages).is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn tmux_fallback_loads_minimal_tpm_plugin_through_shim() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let harness = CliHarness::new("tmux-fallback-tpm")?;
    let home = harness.tmpdir().join("home");
    let tpm_dir = home.join(".tmux/plugins/tpm");
    let spaced_plugin_dir = home.join(".tmux/plugins/plugin with spaces");
    let nested_plugin_dir = home.join(".tmux/plugins/nested");
    fs::create_dir_all(&tpm_dir)?;
    fs::create_dir_all(&spaced_plugin_dir)?;
    fs::create_dir_all(&nested_plugin_dir)?;
    let tpm = tpm_dir.join("tpm");
    fs::write(
        &tpm,
        "#!/bin/sh\n\
         set -eu\n\
         case \"$(tmux -V)\" in tmux\\ *) tmux set-option -g @tpm-version-ok yes ;; *) tmux set-option -g @tpm-version-ok no ;; esac\n\
         tmux set-option -g @tpm-loaded yes\n\
         tmux source-file \"$HOME/.tmux/plugins/plugin with spaces/plugin.tmux\"\n\
         tmux source-file \"$HOME/.tmux/plugins/nested/nested.tmux\"\n\
         tmux set-option -g @plugin-status JOB\n\
         tmux set-option -g status-right 'X#(tmux show-options -gqv @plugin-status)Y'\n",
    )?;
    let mut permissions = fs::metadata(&tpm)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&tpm, permissions)?;
    fs::write(
        spaced_plugin_dir.join("plugin.tmux"),
        "set -g @spaced-plugin yes\n\
         set-hook -g after-new-window 'set -g @hook-plugin-fired yes'\n",
    )?;
    fs::write(
        nested_plugin_dir.join("nested.tmux"),
        "set -g @nested-source yes\nsource-file ~/.tmux/plugins/nested/deeper.tmux\n",
    )?;
    fs::write(
        nested_plugin_dir.join("deeper.tmux"),
        "set -g @recursive-source yes\n",
    )?;
    fs::write(
        home.join(".tmux.conf"),
        "set -g @plugin 'tmux-plugins/tpm'\n\
         set -g @plugin 'local/plugin with spaces'\n\
         run-shell '~/.tmux/plugins/tpm/tpm'\n",
    )?;

    assert_success(
        &harness.run_with(&["new-session", "-d", "-s", "plugins"], |command| {
            command.env_remove("RMUX_DISABLE_TMUX_FALLBACK");
        })?,
    );

    wait_for_option_value(&harness, "@tpm-loaded", "yes")?;
    wait_for_option_value(&harness, "@tpm-version-ok", "yes")?;
    wait_for_option_value(&harness, "@spaced-plugin", "yes")?;
    wait_for_option_value(&harness, "@nested-source", "yes")?;
    wait_for_option_value(&harness, "@recursive-source", "yes")?;
    wait_for_option_value(
        &harness,
        "status-right",
        "X#(tmux show-options -gqv @plugin-status)Y",
    )?;
    assert_success(&harness.run(&["new-window", "-d", "-t", "plugins:", "-n", "hookprobe"])?);
    wait_for_option_value(&harness, "@hook-plugin-fired", "yes")?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn tmux_fallback_tpm_reads_all_plugin_entries_from_tmux_conf() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("tmux-fallback-tpm-plugin-list")?;
    let home = harness.tmpdir().join("home");
    let plugins_root = home.join(".tmux/plugins");
    fs::create_dir_all(&plugins_root)?;

    let tpm_dir = plugins_root.join("tpm");
    fs::create_dir_all(&tpm_dir)?;
    let tpm = tpm_dir.join("tpm");
    fs::write(
        &tpm,
        "#!/bin/sh\n\
         set -eu\n\
         case \"$(tmux -V)\" in tmux\\ *) tmux set-option -g @tpm-version-ok yes ;; *) tmux set-option -g @tpm-version-ok no ;; esac\n\
         tmux set-environment -g TMUX_PLUGIN_MANAGER_PATH \"$HOME/.tmux/plugins/\"\n\
         plugins=$(grep -E '^[[:space:]]*set(-option)?[[:space:]]+-g[[:space:]]+@plugin' \"$HOME/.tmux.conf\" | sed -E \"s/.*@plugin[[:space:]]+['\\\"]?([^'\\\"]+)['\\\"]?.*/\\1/\")\n\
         for plugin in $plugins; do\n\
           name=${plugin##*/}\n\
           for file in \"$HOME/.tmux/plugins/$name\"/*.tmux; do\n\
             [ -f \"$file\" ] || continue\n\
             \"$file\"\n\
           done\n\
         done\n",
    )?;
    make_executable(&tpm)?;

    write_executable_plugin(
        &plugins_root,
        "tmux-sensible",
        "sensible.tmux",
        "#!/bin/sh\n\
         tmux set-option -g @sensible-loaded yes\n\
         tmux bind-key R run-shell \"tmux source-file $HOME/.tmux.conf >/dev/null\"\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "tmux-resurrect",
        "resurrect.tmux",
        "#!/bin/sh\n\
         tmux set-option -g @resurrect-save-script-path \"$HOME/.tmux/plugins/tmux-resurrect/scripts/save.sh\"\n\
         tmux bind-key C-s run-shell \"$HOME/.tmux/plugins/tmux-resurrect/scripts/save.sh\"\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "tmux-continuum",
        "continuum.tmux",
        "#!/bin/sh\n\
         tmux set-option -g @continuum-save-last-timestamp 123\n\
         tmux set-option -g status-right '#(tmux show-options -gqv @continuum-save-last-timestamp)'\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "vim-tmux-navigator",
        "vim-tmux-navigator.tmux",
        "#!/bin/sh\n\
         tmux bind-key -n C-h if-shell \"true\" \"send-keys C-h\" \"select-pane -L\"\n",
    )?;

    fs::write(
        home.join(".tmux.conf"),
        "set -g @plugin 'tmux-plugins/tpm'\n\
         set -g @plugin 'tmux-plugins/tmux-sensible'\n\
         set-option -g @plugin 'tmux-plugins/tmux-resurrect'\n\
         set -g @plugin 'tmux-plugins/tmux-continuum'\n\
         set -g @plugin 'christoomey/vim-tmux-navigator'\n\
         run-shell '~/.tmux/plugins/tpm/tpm'\n",
    )?;

    assert_success(
        &harness.run_with(&["new-session", "-d", "-s", "tpm-list"], |command| {
            command.env_remove("RMUX_DISABLE_TMUX_FALLBACK");
            command.env("HOME", &home);
        })?,
    );

    wait_for_option_value(&harness, "@tpm-version-ok", "yes")?;
    wait_for_environment_value(
        &harness,
        "TMUX_PLUGIN_MANAGER_PATH",
        &format!("{}/", plugins_root.display()),
    )?;
    wait_for_option_value(&harness, "@sensible-loaded", "yes")?;
    wait_for_option_value(
        &harness,
        "@resurrect-save-script-path",
        &format!("{}/tmux-resurrect/scripts/save.sh", plugins_root.display()),
    )?;
    wait_for_option_value(&harness, "@continuum-save-last-timestamp", "123")?;

    let keys = wait_for_list_keys_containing(&harness, "bind-key -T root C-h if-shell")?;
    let normalized_keys = normalize_tmux_table_spaces(&keys);
    for expected in [
        "bind-key -T prefix R run-shell",
        "bind-key -T prefix C-s run-shell",
        "bind-key -T root C-h if-shell",
    ] {
        assert!(
            normalized_keys.contains(expected),
            "expected TPM-loaded binding containing {expected:?}, got:\n{keys}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn tmux_fallback_runs_representative_executable_plugin_scripts() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("tmux-fallback-common-plugins")?;
    let home = harness.tmpdir().join("home");
    let fake_bin = harness.tmpdir().join("fake-bin");
    let external_tmux_marker = harness.tmpdir().join("external-tmux-called");
    let plugins_root = home.join(".tmux/plugins");
    fs::create_dir_all(&plugins_root)?;
    fs::create_dir_all(&fake_bin)?;

    let fake_tmux = fake_bin.join("tmux");
    fs::write(
        &fake_tmux,
        format!(
            "#!/bin/sh\nprintf called >> {}\nexit 97\n",
            shell_quote(&external_tmux_marker)
        ),
    )?;
    make_executable(&fake_tmux)?;

    let tpm_dir = plugins_root.join("tpm");
    fs::create_dir_all(&tpm_dir)?;
    let tpm = tpm_dir.join("tpm");
    fs::write(
        &tpm,
        "#!/bin/sh\n\
         set -eu\n\
         tmux set-environment -g TMUX_PLUGIN_MANAGER_PATH \"$HOME/.tmux/plugins/\"\n\
         for plugin in tmux-sensible tmux-resurrect tmux-continuum tmux-yank tmux-open vim-tmux-navigator tmux-status-fixture; do\n\
           for file in \"$HOME/.tmux/plugins/$plugin\"/*.tmux; do\n\
             [ -f \"$file\" ] || continue\n\
             \"$file\"\n\
           done\n\
         done\n",
    )?;
    make_executable(&tpm)?;

    write_executable_plugin(
        &plugins_root,
        "tmux-sensible",
        "sensible.tmux",
        "#!/bin/sh\n\
         tmux list-keys >/dev/null\n\
         tmux bind-key C-b send-prefix\n\
         tmux bind-key R run-shell \"tmux source-file $HOME/.tmux.conf >/dev/null; tmux display-message reloaded\"\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "tmux-resurrect",
        "resurrect.tmux",
        "#!/bin/sh\n\
         tmux set-option -g @resurrect-save-script-path \"$HOME/.tmux/plugins/tmux-resurrect/scripts/save.sh\"\n\
         tmux bind-key C-s run-shell \"$HOME/.tmux/plugins/tmux-resurrect/scripts/save.sh\"\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "tmux-continuum",
        "continuum.tmux",
        "#!/bin/sh\n\
         tmux display-message -p -F '#{start_time}' >/dev/null\n\
         tmux set-option -g @continuum-save-last-timestamp 123\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "tmux-yank",
        "yank.tmux",
        "#!/bin/sh\n\
         tmux bind-key -T copy-mode-vi y send-keys -X copy-pipe-and-cancel \"tmux display-message yank\"\n\
         tmux bind-key Y run-shell -b \"$HOME/.tmux/plugins/tmux-yank/scripts/copy_pane_pwd.sh\"\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "tmux-open",
        "open.tmux",
        "#!/bin/sh\n\
         tmux bind-key -T copy-mode-vi o send-keys -X copy-pipe-and-cancel \"tmux run-shell -b 'cd #{pane_current_path}; printf open >/dev/null'\"\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "vim-tmux-navigator",
        "vim-tmux-navigator.tmux",
        "#!/bin/sh\n\
         tmux bind-key -n C-h if-shell \"true\" \"send-keys C-h\" \"select-pane -L\"\n",
    )?;
    write_executable_plugin(
        &plugins_root,
        "tmux-status-fixture",
        "status.tmux",
        "#!/bin/sh\n\
         tmux set-option -g @plugin-status COMMON\n\
         tmux set-option -g status-left 'X#(tmux show-options -gqv @plugin-status)Y'\n",
    )?;

    fs::write(
        home.join(".tmux.conf"),
        "set -g @plugin 'tmux-plugins/tpm'\n\
         set -g @plugin 'tmux-plugins/tmux-sensible'\n\
         set -g @plugin 'tmux-plugins/tmux-resurrect'\n\
         set -g @plugin 'tmux-plugins/tmux-continuum'\n\
         set -g @plugin 'tmux-plugins/tmux-yank'\n\
         set -g @plugin 'tmux-plugins/tmux-open'\n\
         set -g @plugin 'christoomey/vim-tmux-navigator'\n\
         run-shell '~/.tmux/plugins/tpm/tpm'\n",
    )?;

    let original_path = std::env::var("PATH").unwrap_or_default();
    let fake_first_path = format!("{}:{original_path}", fake_bin.display());
    assert_success(&harness.run_with(
        &["new-session", "-d", "-s", "commonplugins"],
        |command| {
            command.env_remove("RMUX_DISABLE_TMUX_FALLBACK");
            command.env("HOME", &home);
            command.env("PATH", &fake_first_path);
        },
    )?);

    wait_for_environment_value(
        &harness,
        "TMUX_PLUGIN_MANAGER_PATH",
        &format!("{}/", plugins_root.display()),
    )?;
    wait_for_option_value(
        &harness,
        "@resurrect-save-script-path",
        &format!("{}/tmux-resurrect/scripts/save.sh", plugins_root.display()),
    )?;
    wait_for_option_value(&harness, "@continuum-save-last-timestamp", "123")?;
    wait_for_option_value(&harness, "@plugin-status", "COMMON")?;
    wait_for_option_value(
        &harness,
        "status-left",
        "X#(tmux show-options -gqv @plugin-status)Y",
    )?;

    let keys = wait_for_list_keys_containing(&harness, "bind-key -T prefix C-s run-shell")?;
    let normalized_keys = normalize_tmux_table_spaces(&keys);
    for expected in [
        "bind-key -T prefix C-b send-prefix",
        "bind-key -T prefix R run-shell",
        "bind-key -T copy-mode-vi y send-keys -X copy-pipe-and-cancel",
        "bind-key -T copy-mode-vi o send-keys -X copy-pipe-and-cancel",
        "bind-key -T root C-h if-shell",
    ] {
        assert!(
            normalized_keys.contains(expected),
            "expected plugin binding containing {expected:?}, got:\n{keys}"
        );
    }
    assert!(
        !external_tmux_marker.exists(),
        "plugin scripts escaped the per-socket rmux tmux shim"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn tmux_fallback_shim_preempts_external_tmux_in_path() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let harness = CliHarness::new("tmux-fallback-shim-path")?;
    let home = harness.tmpdir().join("home");
    let external_bin = harness.tmpdir().join("external-bin");
    let marker = harness.tmpdir().join("external-tmux-called");
    fs::create_dir_all(&external_bin)?;
    let fake_tmux = external_bin.join("tmux");
    fs::write(
        &fake_tmux,
        format!(
            "#!/bin/sh\nprintf called >> {}\nexit 99\n",
            shell_quote(&marker)
        ),
    )?;
    let mut permissions = fs::metadata(&fake_tmux)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_tmux, permissions)?;
    fs::write(
        home.join(".tmux.conf"),
        "run-shell 'tmux set-option -g @shim-precedence yes'\n",
    )?;

    let original_path = std::env::var("PATH").unwrap_or_default();
    let fake_first_path = format!("{}:{original_path}", external_bin.display());
    assert_success(
        &harness.run_with(&["new-session", "-d", "-s", "shimpath"], |command| {
            command.env_remove("RMUX_DISABLE_TMUX_FALLBACK");
            command.env("PATH", &fake_first_path);
        })?,
    );

    wait_for_option_value(&harness, "@shim-precedence", "yes")?;
    assert!(
        !marker.exists(),
        "run-shell used external tmux instead of the per-socket rmux shim"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn status_job_plugin_renders_through_tmux_shim() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("status-job-plugin-shim")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "statusjob"])?);
    assert_success(&harness.run(&["set-option", "-g", "@plugin-status", "RENDERED"])?);
    for (option, value) in [
        ("status-left", "X#(tmux show-options -gqv @plugin-status)Y"),
        ("status-left-length", "32"),
        ("status-interval", "1"),
        ("status-right", ""),
        ("window-status-format", ""),
        ("window-status-current-format", ""),
    ] {
        assert_success(&harness.run(&["set-option", "-g", option, value])?);
    }

    let mut attach =
        AttachedSession::spawn(&harness, "statusjob", TerminalSize { cols: 80, rows: 8 })?;
    attach.wait_for_raw_mode(Duration::from_secs(5))?;
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if read_until_contains(
            attach.master_mut(),
            "XRENDEREDY",
            Duration::from_millis(250),
        )
        .is_ok()
        {
            return Ok(());
        }
    }

    Err("status job did not render the tmux shim result".into())
}

#[test]
fn source_file_verbose_prefixes_each_command_with_path_and_line() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-verbose-prefix")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("verbose.conf");
    fs::write(
        &config,
        "set-buffer -b sf yes; display-message -p hello\ndisplay-message -p bye\n",
    )?;

    let output = harness.run(&[
        "source-file",
        "-v",
        config.to_str().expect("utf-8 config path"),
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        format!(
            "{}:1: set-buffer -b sf yes ; display-message -p hello\n{}:2: display-message -p bye\nhello\nbye\n",
            config.display(),
            config.display()
        )
    );
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn source_file_verbose_includes_nested_brace_groups() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-verbose-nested")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("verbose-nested.conf");
    fs::write(
        &config,
        "if-shell -F 1 {\n  display-message -p hi\n}\ndisplay-message -p bye\n",
    )?;

    let output = harness.run(&[
        "source-file",
        "-v",
        config.to_str().expect("utf-8 config path"),
    ])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        format!(
            "{}:2: display-message -p hi\n{}:2: if-shell -F 1 {{ display-message -p hi }}\n{}:4: display-message -p bye\nhi\nbye\n",
            config.display(),
            config.display(),
            config.display(),
        )
    );
    assert!(stderr(&output).is_empty());
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
        format!("No such file or directory: {}\n", missing.display())
    );
    Ok(())
}

#[test]
fn source_file_directory_is_rejected() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-directory")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&[
        "source-file",
        harness.tmpdir().to_str().expect("utf-8 path"),
    ])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        format!("Input/output error: {}\n", harness.tmpdir().display())
    );
    Ok(())
}

#[test]
fn source_file_large_comments_do_not_trip_command_length_limit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-large-comments")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("comments.conf");
    let mut contents = "#\n".repeat(16 * 1024);
    contents.push_str("set-buffer -b source-size ok\n");
    fs::write(&config, contents)?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;
    assert_success(&output);

    let buffer = harness.run(&["show-buffer", "-b", "source-size"])?;
    assert_eq!(buffer.status.code(), Some(0));
    assert_eq!(stdout(&buffer), "ok");
    assert!(stderr(&buffer).is_empty());
    Ok(())
}

#[test]
fn source_file_accepts_long_command_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-long-command")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("long-command.conf");
    let payload = "x".repeat(20 * 1024);
    fs::write(&config, format!("set-buffer -b source-long {payload}\n"))?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;
    assert_success(&output);

    let buffer = harness.run(&["show-buffer", "-b", "source-long"])?;
    assert_eq!(buffer.status.code(), Some(0));
    assert_eq!(stdout(&buffer), payload);
    assert!(stderr(&buffer).is_empty());
    Ok(())
}

#[test]
fn source_file_without_server_does_not_auto_start() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-no-autostart")?;
    let config = harness.tmpdir().join("would-create-session.conf");
    fs::write(&config, "new-session -d -s made_by_source\n")?;

    let output = harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("server") || stderr(&output).contains("error connecting to"),
        "source-file must fail before starting a server, got stderr: {:?}",
        stderr(&output)
    );

    let list = harness.run(&["list-sessions"])?;
    assert_eq!(list.status.code(), Some(1));
    assert!(stdout(&list).is_empty());
    assert!(
        stderr(&list).contains("server") || stderr(&list).contains("error connecting to"),
        "no daemon should be left behind by source-file, got stderr: {:?}",
        stderr(&list)
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

    let baseline_layout = stdout(&windows);
    assert_success(&harness.run(&["if-shell", "-F", "1", "select-layout -n -t alpha:0"])?);
    let next_layout =
        harness.run(&["display-message", "-p", "-t", "alpha:0", "#{window_layout}"])?;
    assert_ne!(stdout(&next_layout), baseline_layout);
    assert!(stderr(&next_layout).is_empty());

    assert_success(&harness.run(&["if-shell", "-F", "1", "select-layout -p -t alpha:0"])?);
    let previous_layout =
        harness.run(&["display-message", "-p", "-t", "alpha:0", "#{window_layout}"])?;
    assert_eq!(stdout(&previous_layout), baseline_layout);
    assert!(stderr(&previous_layout).is_empty());

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
fn show_options_dollar_values_round_trip_through_source_file() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("show-options-dollar-roundtrip")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-option", "-g", "@path", "$HOME/bin"])?);
    let shown = harness.run(&["show-options", "-g", "@path"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), "@path \"\\$HOME/bin\"\n");
    assert!(stderr(&shown).is_empty());

    let source = harness.tmpdir().join("roundtrip.conf");
    fs::write(&source, format!("set-option -g {}", stdout(&shown)))?;
    assert_success(&harness.run(&["set-option", "-g", "@path", "changed"])?);
    assert_success(&harness.run(&["source-file", source.to_str().expect("utf-8 path")])?);

    let value = harness.run(&["show-options", "-gqv", "@path"])?;
    assert_eq!(value.status.code(), Some(0));
    assert_eq!(stdout(&value), "$HOME/bin\n");
    assert!(stderr(&value).is_empty());

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

    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "window-resized",
        "set-buffer -b resized yes",
    ])?);
    assert_success(&harness.run(&["resize-window", "-t", "alpha:0", "-x", "90", "-y", "24"])?);
    wait_for_buffer(&harness, "resized", "yes")?;

    let output = harness.run(&["show-hooks", "-t", "al", "client-attached"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "client-attached[0] display-message hi\n");
    assert!(stderr(&output).is_empty());

    let bindings = harness.run(&["list-keys", "-T", "prefix", "C-b"])?;
    assert_eq!(bindings.status.code(), Some(0));
    assert_eq!(stdout(&bindings), "");
    assert!(stderr(&bindings).is_empty());

    Ok(())
}

#[test]
fn cli_set_and_show_hook_without_scope_use_the_hooks_natural_window() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("hook-natural-window-cli")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "window-layout-changed",
        "set-buffer -b natural-cli yes",
    ])?);
    let shown = harness.run(&["show-hooks", "window-layout-changed"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(
        stdout(&shown),
        "window-layout-changed[0] set-buffer -b natural-cli yes\n"
    );

    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    wait_for_buffer(&harness, "natural-cli", "yes")?;
    Ok(())
}

#[test]
fn source_file_set_hook_without_scope_uses_the_hooks_natural_window() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("hook-natural-window-source")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("natural-window-hook.conf");
    fs::write(
        &config,
        "set-hook window-layout-changed 'set-buffer -b natural-source yes'\n",
    )?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["source-file", config.to_str().expect("utf-8 config")])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    wait_for_buffer(&harness, "natural-source", "yes")?;
    Ok(())
}

#[test]
fn source_file_compact_hook_target_flags_resolve_active_targets() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-compact-window-target")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("compact-window-hook.conf");
    fs::write(
        &config,
        "set-hook -wt1 window-layout-changed 'set-buffer -b compact-window yes'\n",
    )?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["source-file", config.to_str().expect("utf-8 config")])?);

    let active = harness.run(&["show-hooks", "-w", "-t", "alpha:0", "window-layout-changed"])?;
    assert_eq!(
        stdout(&active),
        "window-layout-changed[0] set-buffer -b compact-window yes\n"
    );
    let inactive = harness.run(&["show-hooks", "-w", "-t", "alpha:1", "window-layout-changed"])?;
    assert_eq!(inactive.status.code(), Some(0));
    assert_eq!(stdout(&inactive), "");

    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    wait_for_buffer(&harness, "compact-window", "yes")?;

    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:1.1"])?);
    fs::write(
        &config,
        concat!(
            "set-hook -ptalpha:1 pane-mode-changed 'set-buffer -b compact-pane yes'\n",
            "show-hooks -ptalpha:1 pane-mode-changed\n",
        ),
    )?;
    let sourced = harness.run(&["source-file", config.to_str().expect("utf-8 config")])?;
    assert_eq!(sourced.status.code(), Some(0));
    assert_eq!(
        stdout(&sourced),
        "pane-mode-changed[0] set-buffer -b compact-pane yes\n"
    );
    assert!(stderr(&sourced).is_empty());

    let inactive = harness.run(&["show-hooks", "-p", "-t", "alpha:1.0", "pane-mode-changed"])?;
    assert_eq!(inactive.status.code(), Some(0));
    assert_eq!(stdout(&inactive), "");
    let active = harness.run(&["show-hooks", "-p", "-t", "alpha:1.1", "pane-mode-changed"])?;
    assert_eq!(
        stdout(&active),
        "pane-mode-changed[0] set-buffer -b compact-pane yes\n"
    );
    Ok(())
}

#[test]
fn attached_hook_targets_resolve_as_target_panes_across_entry_paths() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("hook-attached-target-pane")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "foo"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "paw"])?);

    assert_success(&harness.run(&[
        "set-hook",
        "-tfoo",
        "session-renamed",
        "display-message attached-session",
    ])?);
    let session = harness.run(&["show-hooks", "-talpha", "session-renamed"])?;
    let session_by_window = harness.run(&["show-hooks", "-tfoo", "session-renamed"])?;
    assert_eq!(session.status.code(), Some(0));
    assert_eq!(session_by_window.status.code(), Some(0));
    assert_eq!(
        stdout(&session),
        "session-renamed[0] display-message attached-session\n"
    );
    assert_eq!(stdout(&session_by_window), stdout(&session));

    assert_success(&harness.run(&[
        "set-hook",
        "-tfoo",
        "window-layout-changed",
        "set-buffer -b attached-direct yes",
    ])?);
    let direct = harness.run(&["show-hooks", "-tfoo", "window-layout-changed"])?;
    assert_eq!(direct.status.code(), Some(0));
    assert_eq!(
        stdout(&direct),
        "window-layout-changed[0] set-buffer -b attached-direct yes\n"
    );

    assert_success(&harness.run(&[
        "if-shell",
        "-F",
        "1",
        "set-hook -tpaw window-layout-changed 'set-buffer -b attached-queue yes'",
    ])?);
    let queued = harness.run(&["show-hooks", "-w", "-t", "alpha:1", "window-layout-changed"])?;
    assert_eq!(queued.status.code(), Some(0));
    assert_eq!(
        stdout(&queued),
        "window-layout-changed[0] set-buffer -b attached-queue yes\n"
    );

    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    wait_for_buffer(&harness, "attached-direct", "yes")?;
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:1"])?);
    wait_for_buffer(&harness, "attached-queue", "yes")?;
    Ok(())
}

#[test]
fn explicit_window_hook_numeric_target_resolves_a_pane_in_the_current_window(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-window-numeric-pane-target")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "zero"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "one"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0"])?);

    assert_success(&harness.run(&[
        "set-hook",
        "-w",
        "-t1",
        "window-layout-changed",
        "display-message numeric-pane-target",
    ])?);

    let current = harness.run(&["show-hooks", "-w", "-t", "alpha:0", "window-layout-changed"])?;
    assert_eq!(current.status.code(), Some(0));
    assert_eq!(
        stdout(&current),
        "window-layout-changed[0] display-message numeric-pane-target\n"
    );
    let other = harness.run(&["show-hooks", "-w", "-t", "alpha:1", "window-layout-changed"])?;
    assert_eq!(other.status.code(), Some(0));
    assert_eq!(stdout(&other), "");

    let numeric = harness.run(&["show-hooks", "-w", "-t1", "window-layout-changed"])?;
    assert_eq!(numeric.status.code(), Some(0));
    assert_eq!(stdout(&numeric), stdout(&current));
    Ok(())
}

#[test]
fn pane_hooks_use_window_scope_naturally_and_pane_scope_only_with_explicit_p(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-pane-natural-window-scope")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.1"])?);

    assert_success(&harness.run(&[
        "set-hook",
        "-talpha:0.1",
        "pane-mode-changed[0]",
        "display-message natural-target",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "pane-mode-changed[2]",
        "display-message natural-current",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-p",
        "-talpha:0.1",
        "pane-mode-changed[1]",
        "display-message explicit-pane",
    ])?);

    let window = harness.run(&["show-hooks", "-w", "-t", "alpha:0", "pane-mode-changed"])?;
    assert_eq!(window.status.code(), Some(0));
    assert_eq!(
        stdout(&window),
        concat!(
            "pane-mode-changed[0] display-message natural-target\n",
            "pane-mode-changed[2] display-message natural-current\n",
        )
    );
    let natural = harness.run(&["show-hooks", "pane-mode-changed"])?;
    assert_eq!(natural.status.code(), Some(0));
    assert_eq!(stdout(&natural), stdout(&window));

    let pane = harness.run(&["show-hooks", "-p", "-t", "alpha:0.1", "pane-mode-changed"])?;
    assert_eq!(pane.status.code(), Some(0));
    assert_eq!(
        stdout(&pane),
        "pane-mode-changed[1] display-message explicit-pane\n"
    );
    Ok(())
}

#[test]
fn set_hook_run_immediately_dispatches_from_target_pane_regardless_of_scope_flags(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-run-immediately-target-pane")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("run-pane-hook.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-w",
        "-t",
        "alpha:0",
        "pane-mode-changed",
        "set-buffer -b run-window window",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-p",
        "-t",
        "alpha:0.1",
        "pane-mode-changed",
        "set-buffer -b run-pane pane",
    ])?);

    assert_success(&harness.run(&[
        "set-hook",
        "-R",
        "-g",
        "-w",
        "-p",
        "-t",
        "alpha:0.1",
        "pane-mode-changed",
    ])?);
    wait_for_buffer(&harness, "run-pane", "pane")?;

    assert_success(&harness.run(&[
        "set-hook",
        "-u",
        "-p",
        "-t",
        "alpha:0.1",
        "pane-mode-changed",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-R",
        "-p",
        "-t",
        "alpha:0.1",
        "pane-mode-changed",
    ])?);
    wait_for_buffer(&harness, "run-window", "window")?;

    assert_success(&harness.run(&[
        "set-hook",
        "-p",
        "-t",
        "alpha:0.1",
        "pane-mode-changed",
        "set-buffer -b run-source source",
    ])?);
    fs::write(&config, "set-hook -Rgwp -t alpha:0.1 pane-mode-changed\n")?;
    assert_success(&harness.run(&["source-file", config.to_str().expect("utf-8 config")])?);
    wait_for_buffer(&harness, "run-source", "source")?;
    Ok(())
}

#[test]
fn set_hook_run_immediately_missing_target_errors_product_divergence() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("hook-run-missing-target")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("run-missing-hook.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let direct = harness.run(&["set-hook", "-R", "-t", "missing", "pane-mode-changed"])?;
    assert_eq!(direct.status.code(), Some(1));
    assert!(stderr(&direct).contains("can't find pane"));

    fs::write(&config, "set-hook -R -t missing pane-mode-changed\n")?;
    let sourced = harness.run(&["source-file", config.to_str().expect("utf-8 config")])?;
    assert_eq!(sourced.status.code(), Some(1));
    assert!(stderr(&sourced).contains("can't find pane"));
    Ok(())
}

#[test]
fn set_hook_run_immediately_rejects_ignored_mutations_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-run-rejects-ignored-fields")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("run-invalid-hook.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    for arguments in [
        vec!["set-hook", "-Ra", "pane-mode-changed"],
        vec!["set-hook", "-Ru", "pane-mode-changed"],
        vec!["set-hook", "-R", "pane-mode-changed[1]"],
        vec![
            "set-hook",
            "-R",
            "pane-mode-changed",
            "display-message ignored",
        ],
    ] {
        let output = harness.run(&arguments)?;
        assert_eq!(output.status.code(), Some(1), "arguments: {arguments:?}");
        assert!(
            stderr(&output).contains("set-hook -R only accepts a hook name"),
            "arguments: {arguments:?}, stderr: {:?}",
            stderr(&output)
        );
    }

    fs::write(
        &config,
        "set-hook -Rau pane-mode-changed 'display-message ignored'\n",
    )?;
    let sourced = harness.run(&["source-file", config.to_str().expect("utf-8 config")])?;
    assert_eq!(sourced.status.code(), Some(1));
    assert!(stderr(&sourced).contains("set-hook -R only accepts a hook name"));
    Ok(())
}

#[test]
fn show_hooks_without_a_hook_filter_uses_the_target_panes_session_scope(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("show-hooks-unfiltered-session-scope")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "foo"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-tfoo",
        "session-renamed",
        "display-message session-sentinel",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-w",
        "-tfoo",
        "window-layout-changed",
        "display-message window-sentinel",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-p",
        "-tfoo",
        "pane-mode-changed",
        "display-message pane-sentinel",
    ])?);

    let shown = harness.run(&["show-hooks", "-tfoo"])?;
    assert_eq!(shown.status.code(), Some(0));
    let shown = stdout(&shown);
    assert!(shown.contains("session-renamed[0] display-message session-sentinel\n"));
    assert!(!shown.contains("window-sentinel"));
    assert!(!shown.contains("pane-sentinel"));
    Ok(())
}

#[test]
fn set_hook_global_target_is_still_resolved_product_divergence() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-global-target-resolution")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("global-target-hook.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "existing"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "-t",
        "existing",
        "session-renamed",
        "display-message resolved-global",
    ])?);

    let missing = harness.run(&[
        "set-hook",
        "-g",
        "-t",
        "missing",
        "session-renamed",
        "display-message must-not-install",
    ])?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("can't find pane"));

    fs::write(
        &config,
        "set-hook -g -t missing session-renamed 'display-message must-not-install'\n",
    )?;
    let missing_source = harness.run(&["source-file", config.to_str().expect("utf-8 config")])?;
    assert_eq!(missing_source.status.code(), Some(1));
    assert!(stderr(&missing_source).contains("can't find pane"));

    assert_success(&harness.run(&["new-session", "-d", "-s", "ambiguous-one"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "ambiguous-two"])?);
    let ambiguous = harness.run(&[
        "set-hook",
        "-g",
        "-t",
        "ambiguous",
        "session-renamed",
        "display-message must-not-install",
    ])?;
    assert_eq!(ambiguous.status.code(), Some(1));
    assert!(stderr(&ambiguous).contains("ambiguous"));
    Ok(())
}

#[test]
fn filtered_global_show_hooks_normalize_scope_flags_by_hook_class() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("hook-global-filtered-scope")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("global-filtered-show.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "session-renamed",
        "display-message global-session",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "window-layout-changed",
        "display-message global-window",
    ])?);

    for flags in [["-g", "-w"], ["-g", "-p"]] {
        let session = harness.run(&["show-hooks", flags[0], flags[1], "session-renamed"])?;
        assert_eq!(
            stdout(&session),
            "session-renamed[0] display-message global-session\n"
        );
        let window = harness.run(&["show-hooks", flags[0], flags[1], "window-layout-changed"])?;
        assert_eq!(
            stdout(&window),
            "window-layout-changed[0] display-message global-window\n"
        );
    }

    fs::write(
        &config,
        concat!(
            "show-hooks -gw session-renamed\n",
            "show-hooks -gp window-layout-changed\n",
        ),
    )?;
    let sourced = harness.run(&["source-file", config.to_str().expect("utf-8 config")])?;
    assert_eq!(
        stdout(&sourced),
        concat!(
            "session-renamed[0] display-message global-session\n",
            "window-layout-changed[0] display-message global-window\n",
        )
    );
    Ok(())
}

#[test]
fn source_file_set_hook_accepts_compact_flag_clusters() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-hook-compact-flags")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("hooks.conf");
    fs::write(
        &config,
        concat!(
            "set-hook -ga after-new-window 'set-option -g @compact-hook yes'\n",
            "set-hook -gat alpha after-new-window 'set-option -g @compact-target-hook yes'\n",
            "set-hook -pt alpha:0.0 pane-died 'display-message pane-died'\n",
            "set-hook -R client-attached\n",
        ),
    )?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["source-file", config.to_str().expect("utf-8 config")])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha"])?);

    wait_for_option_value(&harness, "@compact-hook", "yes")?;
    wait_for_option_value(&harness, "@compact-target-hook", "yes")?;

    Ok(())
}

#[test]
fn source_file_hook_preserves_double_quotes_and_backslashes_exactly() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("source-hook-quote-round-trip")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let config = harness.tmpdir().join("hook-quotes.conf");
    fs::write(
        &config,
        r#"set-hook -g after-new-window { set-buffer -b got 'a"b\\c' }
"#,
    )?;

    assert_success(&harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha"])?);
    let shown = harness.run(&["show-buffer", "-b", "got"])?;

    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), r#"a"b\\c"#);
    assert!(stderr(&shown).is_empty());
    Ok(())
}

#[test]
fn source_file_nested_assignments_apply_once_at_parse_time_across_all_command_blocks(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-nested-parse-time-assignments")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let config = harness.tmpdir().join("nested-assignments.conf");
    fs::write(
        &config,
        "set-hook -g after-new-window { HOOK_NESTED=hooked }\n\
         if-shell -F 0 { FALSE_BRANCH=parsed }\n\
         if-shell -F 1 { TRUE_BRANCH=parsed } { ELSE_BRANCH=parsed }\n",
    )?;

    assert_success(&harness.run(&["source-file", config.to_str().expect("utf-8 config path")])?);
    for (name, value) in [
        ("HOOK_NESTED", "hooked"),
        ("FALSE_BRANCH", "parsed"),
        ("TRUE_BRANCH", "parsed"),
        ("ELSE_BRANCH", "parsed"),
    ] {
        let shown = harness.run(&["show-environment", "-g", name])?;
        assert_eq!(shown.status.code(), Some(0));
        assert!(stderr(&shown).is_empty());
        assert_eq!(stdout(&shown), format!("{name}={value}\n"));
    }

    assert_success(&harness.run(&[
        "set-environment",
        "-g",
        "HOOK_NESTED",
        "changed-after-parse",
    ])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha"])?);
    let shown = harness.run(&["show-environment", "-g", "HOOK_NESTED"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert!(stderr(&shown).is_empty());
    assert_eq!(stdout(&shown), "HOOK_NESTED=changed-after-parse\n");
    Ok(())
}

#[test]
fn source_file_set_hook_run_immediately_uses_current_session_scope() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-hook-run-immediate-current-session")?;
    let _daemon = harness.start_hidden_daemon()?;
    let config = harness.tmpdir().join("run-hook.conf");
    fs::write(&config, "set-hook -R client-attached\n")?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-t",
        "alpha",
        "client-attached",
        "set-buffer -b run-hook yes",
    ])?);
    assert_success(&harness.run(&["source-file", config.to_str().expect("utf-8 config")])?);

    wait_for_buffer(&harness, "run-hook", "yes")?;
    Ok(())
}

#[test]
fn startup_config_set_hook_without_sessions_does_not_abort_file() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("startup-set-hook-no-session")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let config = harness.tmpdir().join("startup-hooks.conf");
    fs::write(
        &config,
        concat!(
            "set -g @a 1\n",
            "set-hook client-attached 'display-message startup-hook'\n",
            "set-hook -R client-attached\n",
            "set -g @b 2\n",
        ),
    )?;

    let output = harness.run(&[
        "-f",
        config.to_str().expect("utf-8 config"),
        "new-session",
        "-d",
        "-s",
        "y",
    ])?;
    assert_success(&output);

    let show_a = harness.run(&["show-options", "-gv", "@a"])?;
    assert_eq!(show_a.status.code(), Some(0));
    assert!(stderr(&show_a).is_empty());
    assert_eq!(stdout(&show_a), "1\n");
    let show_b = harness.run(&["show-options", "-gv", "@b"])?;
    assert_eq!(show_b.status.code(), Some(0));
    assert!(stderr(&show_b).is_empty());
    assert_eq!(stdout(&show_b), "2\n");
    let hook = harness.run(&["show-hooks", "-g", "client-attached"])?;
    assert_eq!(hook.status.code(), Some(0));
    assert!(stderr(&hook).is_empty());
    assert_eq!(
        stdout(&hook),
        "client-attached[0] display-message startup-hook\n"
    );

    Ok(())
}

#[test]
fn startup_config_set_option_pg_without_sessions_is_noop() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("startup-set-option-pg-no-session")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let config = harness.tmpdir().join("startup-set-option-pg.conf");
    fs::write(
        &config,
        "set -pg pane-border-style fg=colour3\nset -g @after-set-pg yes\n",
    )?;

    let output = harness.run(&[
        "-f",
        config.to_str().expect("utf-8 config"),
        "new-session",
        "-d",
        "-s",
        "alpha",
    ])?;
    assert_success(&output);

    let after = harness.run(&["show-options", "-gqv", "@after-set-pg"])?;
    assert_eq!(after.status.code(), Some(0));
    assert!(stderr(&after).is_empty());
    assert_eq!(stdout(&after), "yes\n");

    let global = harness.run(&["show-options", "-gqv", "pane-border-style"])?;
    assert_eq!(global.status.code(), Some(0));
    assert!(stderr(&global).is_empty());
    assert_eq!(stdout(&global), "default\n");

    let pane = harness.run(&[
        "show-options",
        "-pqv",
        "-t",
        "alpha:0.0",
        "pane-border-style",
    ])?;
    assert_eq!(pane.status.code(), Some(0));
    assert!(stderr(&pane).is_empty());
    assert!(stdout(&pane).is_empty());

    Ok(())
}

#[test]
fn set_hook_accepts_tmux_37_scope_forms() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-hook-tmux-37-scopes")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    for args in [
        vec!["set-hook", "-w", "session-created", "display-message ok"],
        vec!["set-hook", "-p", "client-detached", "display-message ok"],
        vec!["set-hook", "-R", "client-attached"],
        vec![
            "set-hook",
            "-gt",
            "alpha",
            "client-attached",
            "display-message ok",
        ],
        vec![
            "set-hook",
            "-pt",
            "alpha:0.0",
            "pane-died",
            "display-message ok",
        ],
    ] {
        let output = harness.run(&args)?;
        assert_eq!(output.status.code(), Some(0), "args={args:?}");
        assert!(stderr(&output).is_empty(), "args={args:?}");
    }

    Ok(())
}

#[test]
fn show_hooks_global_filter_finds_window_and_pane_hooks() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("show-hooks-global-filter")?;
    let _daemon = harness.start_hidden_daemon()?;
    let hooks = [
        "pane-focus-in",
        "pane-focus-out",
        "pane-exited",
        "pane-died",
        "pane-set-clipboard",
        "window-layout-changed",
        "window-resized",
    ];

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    for hook in hooks {
        assert_success(&harness.run(&["set-hook", "-g", hook, "display-message hooked"])?);

        let output = harness.run(&["show-hooks", "-g", hook])?;

        assert_eq!(
            output.status.code(),
            Some(0),
            "show-hooks failed for {hook}"
        );
        assert_eq!(
            stdout(&output),
            format!("{hook}[0] display-message hooked\n")
        );
        assert!(stderr(&output).is_empty());
    }

    Ok(())
}

#[test]
fn set_hook_canonicalizes_and_validates_command_body() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-hook-command-body")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let display_alias = harness.run(&["set-hook", "-g", "pane-exited", "display hi"])?;
    assert_success(&display_alias);
    let displayed = harness.run(&["show-hooks", "-g", "pane-exited"])?;
    assert_eq!(displayed.status.code(), Some(0));
    assert_eq!(stdout(&displayed), "pane-exited[0] display-message hi\n");
    assert!(stderr(&displayed).is_empty());

    let select_alias = harness.run(&["set-hook", "-g", "pane-exited", "selectw -t :0"])?;
    assert_success(&select_alias);
    let selected = harness.run(&["show-hooks", "-g", "pane-exited"])?;
    assert_eq!(selected.status.code(), Some(0));
    assert_eq!(stdout(&selected), "pane-exited[0] select-window -t :0\n");
    assert!(stderr(&selected).is_empty());

    let invalid = harness.run(&["set-hook", "-g", "pane-exited", "next \"win\""])?;
    assert_eq!(invalid.status.code(), Some(1));
    assert!(stdout(&invalid).is_empty());
    assert!(
        stderr(&invalid).contains("too many arguments"),
        "stderr={:?}",
        stderr(&invalid)
    );

    let unknown = harness.run(&["set-hook", "-g", "pane-exited", "not-a-command"])?;
    assert_eq!(unknown.status.code(), Some(1));
    assert!(stdout(&unknown).is_empty());
    assert!(
        stderr(&unknown).contains("unknown command: not-a-command"),
        "stderr={:?}",
        stderr(&unknown)
    );

    Ok(())
}

#[test]
fn pane_died_hook_fires_for_remain_on_exit_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("pane-died-hook")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-g", "remain-on-exit", "on"])?);
    assert_success(&harness.run(&["set-hook", "-g", "pane-died", "set-buffer -b died yes"])?);
    assert_success(&harness.run(&["split-window", "-t", "alpha", "exit 0"])?);

    wait_for_buffer(&harness, "died", "yes")?;

    Ok(())
}

#[test]
fn window_unlinked_hook_fires_when_last_pane_exits() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("window-unlinked-last-pane-exit")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "keep"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "window-unlinked",
        "set-buffer -b unlinked yes",
    ])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", "-n", "gone", "exit 0"])?);

    wait_for_buffer(&harness, "unlinked", "yes")?;

    Ok(())
}

fn wait_for_buffer(
    harness: &CliHarness,
    buffer: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let output = harness.run(&["show-buffer", "-b", buffer])?;
        if output.status.code() == Some(0) && stdout(&output).trim_end() == expected {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for buffer {buffer}={expected:?}; last status={:?} stdout={:?} stderr={:?}",
                output.status.code(),
                stdout(&output),
                stderr(&output)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
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

fn wait_for_file(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if path.is_file() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("timed out waiting for {}", path.display()).into())
}

fn wait_for_file_text(path: &std::path::Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(contents) => last = Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!(
        "timed out waiting for {} to contain {expected:?}; last={last:?}",
        path.display()
    )
    .into())
}

fn run_concurrent_cli(
    harness: &CliHarness,
    arguments: &[&str],
) -> Result<std::process::Output, Box<dyn Error>> {
    // These lifecycle probes intentionally overlap a blocking run-shell request.
    // The suite-wide CLI lock is for sequential probes and can delay this
    // mutation until after the shell has completed under full-suite load.
    let mut command = harness.base_command();
    command.args(arguments).stdin(Stdio::null());
    Ok(command.output()?)
}

fn wait_for_option_value(
    harness: &CliHarness,
    option: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while std::time::Instant::now() < deadline {
        let output = harness.run(&["show-options", "-gqv", option])?;
        if output.status.code() == Some(0) && stdout(&output).trim_end() == expected {
            return Ok(());
        }
        last = Some(output);
        std::thread::sleep(Duration::from_millis(20));
    }
    let last = last.expect("show-options was attempted");
    Err(format!(
        "timed out waiting for option {option}={expected:?}; status={:?} stdout={:?} stderr={:?}",
        last.status.code(),
        stdout(&last),
        stderr(&last)
    )
    .into())
}

fn wait_for_environment_value(
    harness: &CliHarness,
    name: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while std::time::Instant::now() < deadline {
        let output = harness.run(&["show-environment", "-g", name])?;
        if output.status.code() == Some(0)
            && stdout(&output).trim_end() == format!("{name}={expected}")
        {
            return Ok(());
        }
        last = Some(output);
        std::thread::sleep(Duration::from_millis(20));
    }
    let last = last.expect("show-environment was attempted");
    Err(format!(
        "timed out waiting for environment {name}={expected:?}; status={:?} stdout={:?} stderr={:?}",
        last.status.code(),
        stdout(&last),
        stderr(&last)
    )
    .into())
}

fn wait_for_list_keys_containing(
    harness: &CliHarness,
    expected: &str,
) -> Result<String, Box<dyn Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while std::time::Instant::now() < deadline {
        let output = harness.run(&["list-keys"])?;
        if output.status.code() == Some(0)
            && normalize_tmux_table_spaces(&stdout(&output)).contains(expected)
        {
            return Ok(stdout(&output));
        }
        last = Some(output);
        std::thread::sleep(Duration::from_millis(20));
    }
    let last = last.expect("list-keys was attempted");
    Err(format!(
        "timed out waiting for list-keys to contain {expected:?}; status={:?} stdout={:?} stderr={:?}",
        last.status.code(),
        stdout(&last),
        stderr(&last)
    )
    .into())
}

fn normalize_tmux_table_spaces(value: &str) -> String {
    value
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
fn write_executable_plugin(
    plugins_root: &std::path::Path,
    plugin: &str,
    name: &str,
    contents: &str,
) -> Result<(), Box<dyn Error>> {
    let dir = plugins_root.join(plugin);
    fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    fs::write(&path, contents)?;
    make_executable(&path)
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn shell_quote(path: &std::path::Path) -> String {
    shell_quote_str(&path.display().to_string())
}

fn shell_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
