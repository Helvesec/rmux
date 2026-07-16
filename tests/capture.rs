#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::Duration;

use common::{
    assert_success, stderr, stdout, terminate_child, CliHarness, FrozenTmuxBinary,
    TmuxCompatHarness, TmuxCompatRun, TmuxCompatRunConfig, FROZEN_TMUX_ENV,
};
use rmux_proto::DEFAULT_MAX_FRAME_LENGTH;

#[test]
fn capture_pane_prints_unattached_transcript() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-print")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let marker = "cli_capture_print_marker";

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        &format!("printf '{marker}\\n'"),
        "Enter",
    ])?);

    let output = wait_for_capture(&harness, marker)?;
    assert!(stdout(&output).contains(marker));
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn capture_pane_non_numeric_bounds_are_rejected() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-nonnumeric-bounds")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "printf 'line1\\nline2\\n'; sleep 60",
    ])?);
    let _ = wait_for_capture(&harness, "line2")?;

    let direct = harness.run(&["capture-pane", "-p", "-t", "alpha:0.0", "-S", "abc"])?;
    assert_eq!(direct.status.code(), Some(1));
    assert!(stdout(&direct).is_empty());
    assert_eq!(
        stderr(&direct),
        "command capture-pane: -S expects a number\n"
    );

    let queued = harness.run(&["run-shell", "-C", "capture-pane -p -t alpha:0.0 -S abc"])?;
    assert_eq!(queued.status.code(), Some(1));
    assert!(stdout(&queued).is_empty());
    assert!(
        stderr(&queued).contains("command capture-pane: -S expects a number"),
        "stderr={}",
        stderr(&queued)
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn capture_pane_format_flags_match_tmux_supported_surface() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-pane-format-flags")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let marker = "cli_capture_format_flags_marker";
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        &format!("printf '{marker}\\n'"),
        "Enter",
    ])?);
    let _ = wait_for_capture(&harness, marker)?;

    let line_numbers = harness.run(&["capture-pane", "-p", "-L", "-t", "alpha:0.0"])?;
    assert_eq!(line_numbers.status.code(), Some(0));
    assert!(stderr(&line_numbers).is_empty());
    assert!(
        stdout(&line_numbers)
            .lines()
            .any(|line| line.contains(marker)
                && line
                    .split_once(' ')
                    .and_then(|(prefix, _)| prefix.parse::<i64>().ok())
                    .is_some()),
        "line-numbered output did not prefix the marker line: {:?}",
        stdout(&line_numbers)
    );

    let line_flags = harness.run(&["capture-pane", "-p", "-F", "-t", "alpha:0.0"])?;
    assert_eq!(line_flags.status.code(), Some(0));
    assert!(stderr(&line_flags).is_empty());
    assert!(
        stdout(&line_flags)
            .lines()
            .any(|line| line.contains(marker) && line.starts_with("- ")),
        "line-flag output did not prefix the marker line: {:?}",
        stdout(&line_flags)
    );

    let hyperlinks = harness.run(&["capture-pane", "-p", "-H", "-t", "alpha:0.0"])?;
    assert_eq!(hyperlinks.status.code(), Some(0));
    assert!(stderr(&hyperlinks).is_empty());

    let queued = harness.run(&["run-shell", "-C", "capture-pane -p -L -t alpha:0.0"])?;
    assert_eq!(queued.status.code(), Some(0));
    assert!(stderr(&queued).is_empty());
    assert!(
        stdout(&queued).lines().any(|line| line.contains(marker)),
        "queued capture-pane -L did not return the captured marker: {:?}",
        stdout(&queued)
    );
    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn capture_pane_format_flags_mark_wrapped_lines() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-pane-format-wrapped")?;
    let _cleanup = harness.auto_start_cleanup()?;
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-x",
        "8",
        "-y",
        "4",
        "-s",
        "alpha",
        "printf 'abcdefghijklmnop'; sleep 3",
    ])?);
    let _ = wait_for_capture(&harness, "ijklmnop")?;

    let line_flags = harness.run(&["capture-pane", "-pF", "-t", "alpha:0.0"])?;
    assert_eq!(line_flags.status.code(), Some(0));
    assert!(stderr(&line_flags).is_empty());
    assert!(
        stdout(&line_flags).starts_with("W abcdefgh\n- ijklmnop\n"),
        "wrapped line flags did not match tmux: {:?}",
        stdout(&line_flags)
    );

    Ok(())
}

#[test]
fn capture_pane_hyperlinks_prints_osc8_uri() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-pane-hyperlinks")?;
    let _cleanup = harness.auto_start_cleanup()?;
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "printf '\\033]8;id=abc;https://example.com\\033\\\\link\\033]8;;\\033\\\\\\n'; sleep 3",
    ])?);
    let _ = wait_for_capture(&harness, "link")?;

    let hyperlinks = harness.run(&["capture-pane", "-pH", "-t", "alpha:0.0"])?;
    assert_eq!(hyperlinks.status.code(), Some(0));
    assert!(stderr(&hyperlinks).is_empty());
    assert_eq!(stdout(&hyperlinks), "https://example.com\n");

    Ok(())
}

#[test]
fn capture_pane_writes_buffer_without_printing() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-buffer")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let marker = "cli_capture_buffer_marker";

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        &format!("printf '{marker}\\n'"),
        "Enter",
    ])?);
    let _ = wait_for_capture(&harness, marker)?;

    assert_success(&harness.run(&["capture-pane", "-t", "alpha:0.0", "-b", "cap"])?);

    let show = harness.run(&["show-buffer", "-b", "cap"])?;
    assert_eq!(show.status.code(), Some(0));
    assert!(stdout(&show).contains(marker));
    assert!(stderr(&show).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn capture_pane_print_does_not_reorder_buffers() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-print-buffer-order")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let marker = "cli_capture_order_marker";

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "stable-head"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        &format!("printf '{marker}\\n'"),
        "Enter",
    ])?);
    let _ = wait_for_capture(&harness, marker)?;

    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "stable-head");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn capture_pane_can_print_large_scrollback() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-large-scrollback")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-x",
        "80",
        "-y",
        "24",
        "sleep 60",
    ])?);
    assert_success(&harness.run(&["set-option", "-g", "history-limit", "40000"])?);
    assert_success(&harness.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "alpha:0.0",
        "printf 'rmux-large-capture-start\\n'; dd if=/dev/zero bs=2097152 count=1 2>/dev/null | tr '\\000' A; printf '\\nrmux-large-capture-end\\n'; sleep 60",
    ])?);
    let _ = wait_for_capture(&harness, "rmux-large-capture-end")?;

    let output = harness.run(&[
        "capture-pane",
        "-p",
        "-S",
        "-",
        "-E",
        "-",
        "-t",
        "alpha:0.0",
    ])?;
    let rendered = stdout(&output);

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(rendered.contains("rmux-large-capture-start"));
    assert!(rendered.contains("rmux-large-capture-end"));
    assert!(
        rendered.len() > DEFAULT_MAX_FRAME_LENGTH,
        "large capture stayed below old frame cap: {} bytes",
        rendered.len()
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn capture_pane_matches_frozen_tmux_for_respawned_output_when_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("capture-pane-tmux-compat")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let config = TmuxCompatRunConfig::default();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&create);
    let remain_on_exit = harness.run_pair_with(
        &tmux_binary,
        &["set-window-option", "-t", "alpha:0", "remain-on-exit", "on"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&remain_on_exit);
    let respawn = harness.run_pair_with(
        &tmux_binary,
        &[
            "respawn-pane",
            "-k",
            "-t",
            "alpha:0.0",
            "printf 'capture-tmux-compat-marker\\n'; sleep 10",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&respawn);

    assert_capture_tmux_compat_when(
        &harness,
        &tmux_binary,
        &config,
        &["capture-pane", "-p", "-t", "alpha:0.0"],
        |run| {
            run.tmux
                .stdout_string()
                .contains("capture-tmux-compat-marker")
        },
        "capture-pane compatibility never observed marker",
    )
}

#[test]
fn capture_pane_matches_frozen_tmux_after_respawned_pane_exits_when_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("capture-pane-dead-pane-tmux-compat")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let config = TmuxCompatRunConfig::default();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&create);
    let remain_on_exit = harness.run_pair_with(
        &tmux_binary,
        &["set-window-option", "-t", "alpha:0", "remain-on-exit", "on"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&remain_on_exit);
    let remain_on_exit_format = harness.run_pair_with(
        &tmux_binary,
        &[
            "set-window-option",
            "-t",
            "alpha:0",
            "remain-on-exit-format",
            "Pane is dead (status #{pane_dead_status})",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&remain_on_exit_format);

    let respawn = harness.run_pair_with(
        &tmux_binary,
        &[
            "respawn-pane",
            "-k",
            "-t",
            "alpha:0.0",
            "printf 'one\\ntwo\\n'; sleep 0.2",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&respawn);

    assert_capture_tmux_compat_when(
        &harness,
        &tmux_binary,
        &config,
        &["capture-pane", "-p", "-t", "alpha:0.0"],
        |run| run.tmux.stdout_string().contains("Pane is dead"),
        "capture-pane dead-pane compatibility never observed the dead pane marker",
    )
}

#[test]
fn capture_pane_matches_frozen_tmux_for_escape_range_and_quiet_flags_when_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("capture-pane-flags-tmux-compat")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let config = TmuxCompatRunConfig::default();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&create);
    let remain_on_exit = harness.run_pair_with(
        &tmux_binary,
        &["set-window-option", "-t", "alpha:0", "remain-on-exit", "on"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&remain_on_exit);

    let respawn = harness.run_pair_with(
        &tmux_binary,
        &[
            "respawn-pane",
            "-k",
            "-t",
            "alpha:0.0",
            "printf '\\033[31mcapture-flags-marker\\033[0m  \\nplain\\n'; sleep 10",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&respawn);

    assert_capture_tmux_compat_when(
        &harness,
        &tmux_binary,
        &config,
        &["capture-pane", "-p", "-e", "-C", "-t", "alpha:0.0"],
        |run| {
            run.tmux
                .stdout_string()
                .contains("\\033[31mcapture-flags-marker\\033[39m")
        },
        "capture-pane -e -C compatibility never observed the escaped ANSI marker",
    )?;

    assert_capture_tmux_compat_when(
        &harness,
        &tmux_binary,
        &config,
        &[
            "capture-pane",
            "-p",
            "-S",
            "0",
            "-E",
            "0",
            "-t",
            "alpha:0.0",
        ],
        |run| run.tmux.stdout_string().contains("capture-flags-marker"),
        "capture-pane range compatibility never observed the marker line",
    )?;

    let alternate_quiet = harness.run_pair_with(
        &tmux_binary,
        &["capture-pane", "-p", "-a", "-q", "-t", "alpha:0.0"],
        config,
    )?;
    assert_exact_tmux_compat(&alternate_quiet);
    Ok(())
}

fn wait_for_capture(
    harness: &CliHarness,
    marker: &str,
) -> Result<std::process::Output, Box<dyn Error>> {
    let mut last = None;
    for _ in 0..100 {
        let output = harness.run(&["capture-pane", "-p", "-t", "alpha:0.0"])?;
        if output.status.code() == Some(0) && stdout(&output).contains(marker) {
            return Ok(output);
        }
        last = Some(output);
        std::thread::sleep(Duration::from_millis(20));
    }

    let last = last.expect("capture was attempted");
    Err(format!(
        "capture output never contained marker {marker}; status={:?} stdout={:?} stderr={:?}",
        last.status.code(),
        stdout(&last),
        stderr(&last)
    )
    .into())
}

fn assert_exact_tmux_compat(run: &TmuxCompatRun) {
    assert_eq!(run.tmux.status_code, run.rmux.status_code);
    assert_eq!(run.tmux.timed_out, run.rmux.timed_out);
    assert_eq!(run.tmux.stdout, run.rmux.stdout);
    assert_eq!(run.tmux.stderr, run.rmux.stderr);
}

fn assert_capture_tmux_compat_when<F>(
    harness: &TmuxCompatHarness,
    tmux_binary: &std::path::Path,
    config: &TmuxCompatRunConfig,
    argv: &[&str],
    ready: F,
    context: &str,
) -> Result<(), Box<dyn Error>>
where
    F: Fn(&TmuxCompatRun) -> bool,
{
    let mut last = None;
    for _ in 0..100 {
        let capture = harness.run_pair_with(tmux_binary, argv, config.clone())?;
        if capture.tmux.stdout == capture.rmux.stdout
            && capture.tmux.stderr == capture.rmux.stderr
            && capture.tmux.status_code == capture.rmux.status_code
            && ready(&capture)
        {
            assert_exact_tmux_compat(&capture);
            return Ok(());
        }
        last = Some(capture);
        std::thread::sleep(Duration::from_millis(20));
    }

    let last = last.expect("capture compatibility was attempted");
    Err(format!(
        "{context}; argv={argv:?}; tmux status={:?} stdout={:?} stderr={:?}; rmux status={:?} stdout={:?} stderr={:?}",
        last.tmux.status_code,
        last.tmux.stdout_string(),
        last.tmux.stderr_string(),
        last.rmux.status_code,
        last.rmux.stdout_string(),
        last.rmux.stderr_string()
    )
    .into())
}
