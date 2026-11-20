#![cfg(unix)]

mod common;
mod lifecycle_characterization;

use std::error::Error;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use common::{assert_success, terminate_child, tmux_shim_directories, CliHarness};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
type SharedOutput = Arc<Mutex<Vec<u8>>>;
type OutputCollector = JoinHandle<io::Result<Vec<u8>>>;

#[test]
fn non_attaching_cli_list_sessions_exits_with_open_stdin_and_ignores_follow_on_input(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-cli-list-exit")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);

    let mut control = ControlProcess::spawn(&harness, &["list-sessions"])?;
    control.write_input("display-message -p SHOULD-NOT-RUN\n")?;
    let output = control.wait_for_exit_with_open_stdin()?;

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert!(
        output.stdout.contains("alpha: 1 windows"),
        "{:?}",
        output.stdout
    );
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    assert!(
        !output.stdout.contains("SHOULD-NOT-RUN"),
        "{:?}",
        output.stdout
    );
    Ok(())
}

#[test]
fn non_attaching_cli_new_session_detached_exits_with_open_stdin() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-cli-new-detached-exit")?;
    let _daemon = harness.start_hidden_daemon()?;
    let mut control = ControlProcess::spawn(&harness, &["new-session", "-d", "-s", "alpha"])?;

    let output = control.wait_for_exit_with_open_stdin()?;

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert!(
        output.stdout.contains("%sessions-changed\n"),
        "{:?}",
        output.stdout
    );
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    Ok(())
}

#[test]
fn attaching_cli_attach_session_stays_interactive_until_stdin_eof() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-cli-attach-interactive")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
    let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;

    control.wait_for_output("%session-changed $0 alpha\n")?;
    control.assert_running()?;
    control.write_input("display-message -p FOLLOW-ON\n")?;
    control.wait_for_output("FOLLOW-ON\n")?;
    control.assert_running()?;
    let output = control.close_stdin_and_wait()?;

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert!(output.stdout.contains("FOLLOW-ON\n"), "{:?}", output.stdout);
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    Ok(())
}

#[test]
fn attaching_cli_new_session_stays_interactive_until_stdin_eof() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-cli-new-interactive")?;
    let _daemon = harness.start_hidden_daemon()?;
    let mut control = ControlProcess::spawn(&harness, &["new-session", "-s", "alpha"])?;

    control.wait_for_output("%session-changed $0 alpha\n")?;
    control.assert_running()?;
    control.write_input("display-message -p FOLLOW-ON\n")?;
    control.wait_for_output("FOLLOW-ON\n")?;
    control.assert_running()?;
    let output = control.close_stdin_and_wait()?;

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert!(output.stdout.contains("FOLLOW-ON\n"), "{:?}", output.stdout);
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    Ok(())
}

#[test]
fn control_wait_on_empty_channel_holds_next_reply_until_signal() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b treats '' as an ordinary channel name: the control frame
    // closes with %end and the next reply waits for `wait-for -S ''`.
    let harness = CliHarness::new("control-cli-empty-wait")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
    let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
    control.wait_for_output("%session-changed $0 alpha\n")?;
    control.write_input("wait-for ''\ndisplay-message -p AFTER-WAIT\n")?;
    // The attach frame is the first %end; the wait frame closes the second.
    control.wait_for_occurrences("%end ", 2)?;
    control.assert_running()?;
    assert!(!control.stdout_text().contains("AFTER-WAIT"));
    assert_success(&harness.run(&["wait-for", "-S", ""])?);
    control.wait_for_output("AFTER-WAIT\n")?;
    let output = control.close_stdin_and_wait()?;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert!(!output.stdout.contains("%error "), "{:?}", output.stdout);
    let lines = output.stdout.lines().collect::<Vec<_>>();
    let after = lines
        .iter()
        .position(|line| *line == "AFTER-WAIT")
        .ok_or("next reply missing")?;
    assert!(lines[after + 1].starts_with("%end "), "{:?}", output.stdout);
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    Ok(())
}

#[test]
fn control_invalid_colon_preserves_unknown_flags_literals_frames_and_queue_canaries(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-invalid-colon")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);

    for (literal, diagnostic) in [
        ("-:", "invalid flag -:"),
        ("-:1: literal output", "invalid flag -:"),
        ("-Q", "unknown flag -Q"),
    ] {
        assert_success(&harness.run(&["set-option", "-gu", "@control-flag-canary"])?);
        let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
        control.wait_for_output("%session-changed $0 alpha\n")?;
        control.write_input(&format!(
            "display-message -p '{literal}' ; set-option -g @control-flag-canary unexpected\n\
             display-message -p AFTER-INVALID\n"
        ))?;
        control.wait_for_output("AFTER-INVALID\n")?;
        let canary = harness.run(&["show-options", "-gqv", "@control-flag-canary"])?;
        assert_success(&canary);
        assert!(canary.stdout.is_empty(), "{canary:?}");

        control.write_input(&format!(
            "display-message -p -- '{literal}' ; set-option -g @control-flag-canary expected\n\
             display-message -p AFTER-LITERAL\n"
        ))?;
        control.wait_for_output("AFTER-LITERAL\n")?;
        let output = control.close_stdin_and_wait()?;
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        let lines = output.stdout.lines().collect::<Vec<_>>();
        let error = lines
            .iter()
            .position(|line| line.starts_with("%error "))
            .ok_or_else(|| format!("missing error frame: {:?}", output.stdout))?;
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("%error "))
                .count(),
            1,
            "{:?}",
            output.stdout
        );
        let begin = error.checked_sub(2).ok_or("truncated error frame")?;
        assert!(lines[begin].starts_with("%begin "), "{:?}", output.stdout);
        assert_eq!(
            lines[begin].strip_prefix("%begin "),
            lines[error].strip_prefix("%error ")
        );
        assert_eq!(
            lines[begin + 1],
            format!("parse error: command display-message: {diagnostic}")
        );
        let after = lines
            .iter()
            .position(|line| *line == "AFTER-INVALID")
            .unwrap();
        assert!(after > error, "{:?}", output.stdout);
        let literal_line = lines.iter().position(|line| *line == literal).unwrap();
        assert!(literal_line > after, "{:?}", output.stdout);
        assert_eq!(
            lines[literal_line - 1].strip_prefix("%begin "),
            lines[literal_line + 1].strip_prefix("%end ")
        );
        assert!(lines[literal_line - 1].starts_with("%begin "));
        assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
        let canary = harness.run(&["show-options", "-gqv", "@control-flag-canary"])?;
        assert_eq!(canary.status.code(), Some(0), "{canary:?}");
        assert!(canary.stderr.is_empty(), "{canary:?}");
        assert_eq!(canary.stdout, b"expected\n");
    }
    Ok(())
}

#[test]
fn stdin_control_insertion_refusal_keeps_error_and_stops_children_and_tail(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-cli-insertion-refusal")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
    let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
    control.wait_for_output("%session-changed $0 alpha\n")?;
    let children = vec!["start-server"; 1025].join(" ; ");
    control.write_input(&format!(
        "if-shell -F 1 '{children}' ; display-message -p TAIL\ndisplay-message -p RECOVERED\n"
    ))?;
    control.wait_for_output("RECOVERED\n")?;
    let output = control.close_stdin_and_wait()?;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let lines = output.stdout.lines().collect::<Vec<_>>();
    let error = lines
        .iter()
        .position(|line| line.starts_with("%error "))
        .ok_or_else(|| {
            format!(
                "insertion refusal lost its error guard: {:?}",
                output.stdout
            )
        })?;
    let begin = lines[..error]
        .iter()
        .rposition(|line| line.starts_with("%begin "))
        .ok_or("error guard has no begin")?;
    assert_eq!(
        lines[begin].strip_prefix("%begin "),
        lines[error].strip_prefix("%error ")
    );
    assert!(lines[begin + 1..error].iter().any(|line| line
        .contains("control command queue inserted too many commands: 1025 (maximum 1024)")));
    assert!(!lines.contains(&"TAIL"), "{:?}", output.stdout);
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("%begin "))
            .count(),
        3,
        "children must not run: {:?}",
        output.stdout
    );
    let recovered = lines
        .iter()
        .position(|line| *line == "RECOVERED")
        .ok_or("next input missing")?;
    assert!(recovered > error, "{:?}", output.stdout);
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    Ok(())
}

#[test]
fn control_client_reports_a_server_killed_under_it_like_tmux() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b: `%exit server exited unexpectedly`, exit status 1, no stderr.
    for (mode, prefix, suffix) in [
        ("-C", "", ""),
        (
            "-CC",
            rmux_proto::CONTROL_CONTROL_START,
            rmux_proto::CONTROL_CONTROL_END,
        ),
    ] {
        let harness = CliHarness::new("control-cli-server-killed")?;
        let mut daemon = harness.start_hidden_daemon()?;
        assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
        let mut control =
            ControlProcess::spawn_in_mode(&harness, mode, &["attach-session", "-t", "alpha"])?;
        control.wait_for_output("%session-changed $0 alpha\n")?;

        daemon.child_mut().kill()?;
        daemon.child_mut().wait()?;
        let output = control.wait_for_exit_with_open_stdin()?;
        // SIGKILL leaves the killed daemon's tmux shim behind; remove that one
        // shim so the harness still reports any shim an orderly exit leaks.
        let leaked = tmux_shim_directories(harness.runtime_dir())?;
        assert_eq!(leaked.len(), 1, "{mode}: shims={leaked:?}");
        for shim in leaked {
            std::fs::remove_dir_all(harness.runtime_dir().join(shim))?;
        }

        assert_eq!(output.status.code(), Some(1), "{mode}: {:?}", output.stdout);
        assert!(
            output.stderr.is_empty(),
            "{mode}: stderr={:?}",
            output.stderr
        );
        assert!(
            output.stdout.starts_with(prefix),
            "{mode}: {:?}",
            output.stdout
        );
        let expected_end = format!("\n%exit server exited unexpectedly\n{suffix}");
        assert!(
            output.stdout.ends_with(&expected_end),
            "{mode}: {:?}",
            output.stdout
        );
        assert_eq!(
            output
                .stdout
                .lines()
                .filter(|line| line.starts_with("%exit"))
                .count(),
            1,
            "{mode}: {:?}",
            output.stdout
        );
    }
    Ok(())
}

#[test]
fn control_client_without_a_session_exits_one_when_its_command_fails() -> Result<(), Box<dyn Error>>
{
    // tmux 3.7b (R3511 M2): the server ends a client that joined no session
    // once its command-line commands ran, with status 1 after a failure.
    let harness = CliHarness::new("control-cli-command-failed")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
    for mode in ["-C", "-CC"] {
        for (command_args, expected) in [
            (&["attach-session", "-t", "nope"][..], 1),
            (&["has-session", "-t", "nope"][..], 1),
            (&["new-session", "-s", "alpha"][..], 1),
            (&["has-session", "-t", "alpha"][..], 0),
        ] {
            let mut control = ControlProcess::spawn_in_mode(&harness, mode, command_args)?;
            let output = control.wait_for_exit_with_open_stdin()?;

            assert_eq!(
                output.status.code(),
                Some(expected),
                "{mode} {command_args:?}: {:?}",
                output.stdout
            );
            assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
            assert_eq!(
                output.stdout.contains("%error "),
                expected == 1,
                "{mode} {command_args:?}: {:?}",
                output.stdout
            );
        }
    }
    Ok(())
}

#[test]
fn control_attach_without_a_server_exits_one_with_no_sessions() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b: `-C attach` without a server starts one, reports
    // `no sessions` in an `%error` block and exits 1.
    let harness = CliHarness::new("control-cli-no-sessions")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let mut control = ControlProcess::spawn(&harness, &["attach-session"])?;
    let output = control.wait_for_exit_with_open_stdin()?;

    assert_eq!(output.status.code(), Some(1), "{:?}", output.stdout);
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert!(
        output.stdout.contains("no sessions\n"),
        "{:?}",
        output.stdout
    );
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    Ok(())
}

#[test]
fn control_client_that_detaches_after_a_failed_command_exits_zero() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b: a detach ends the client with MSG_DETACH, which carries no
    // status, so the earlier failure does not reach the exit status.
    let harness = CliHarness::new("control-cli-failed-then-detach")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
    let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
    control.wait_for_output("%session-changed $0 alpha\n")?;
    control.write_input("has-session -t nope\n")?;
    control.wait_for_output("can't find session: nope\n")?;
    control.write_input("detach-client\n")?;
    let output = control.wait_for_exit_with_open_stdin()?;

    assert_eq!(output.status.code(), Some(0), "{:?}", output.stdout);
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert!(output.stdout.ends_with("%exit\n"), "{:?}", output.stdout);
    Ok(())
}

#[test]
fn control_client_keeps_a_clean_exit_on_an_ordinary_kill_server() -> Result<(), Box<dyn Error>> {
    for mode in ["-C", "-CC"] {
        let harness = CliHarness::new("control-cli-kill-server")?;
        let _daemon = harness.start_hidden_daemon()?;
        assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
        let mut control =
            ControlProcess::spawn_in_mode(&harness, mode, &["attach-session", "-t", "alpha"])?;
        control.wait_for_output("%session-changed $0 alpha\n")?;

        assert_success(&harness.run(&["kill-server"])?);
        let output = control.wait_for_exit_with_open_stdin()?;

        assert_eq!(output.status.code(), Some(0), "{mode}: {:?}", output.stdout);
        assert!(
            output.stderr.is_empty(),
            "{mode}: stderr={:?}",
            output.stderr
        );
        assert!(
            !output.stdout.contains("unexpectedly"),
            "{mode}: {:?}",
            output.stdout
        );
        assert_eq!(
            output
                .stdout
                .lines()
                .filter(|line| line.starts_with("%exit"))
                .count(),
            1,
            "{mode}: {:?}",
            output.stdout
        );
    }
    Ok(())
}

#[test]
fn control_client_of_a_server_shutting_down_receives_tmux_stop_block() -> Result<(), Box<dyn Error>>
{
    // tmux 3.7b, measured for SIGTERM, SIGINT and kill-server: one
    // `%sessions-changed` per session the exit closes, then a bare `%exit`,
    // exit status 0, no stderr, `-CC` closing its DCS frame, and the
    // session-closed hook run.
    assert_control_stop_block(&["alpha", "beta"], false)?;
    assert_control_stop_block(&["alpha"], true)
}

#[test]
fn control_client_of_a_server_stopping_two_hooked_sessions_runs_both_hooks_product_divergence(
) -> Result<(), Box<dyn Error>> {
    // Deliberate divergence (C-D44): with two sessions and a blocking
    // run-shell session-closed hook, tmux 3.7b races its client exit against
    // the first hook and usually runs only that hook, so -C usually sees one
    // `%sessions-changed` and -CC a varying block. RMUX tells the client both
    // closes and runs both hooks, the block tmux gives without a hook.
    assert_control_stop_block(&["alpha", "beta"], true)
}

/// Stops a server holding `sessions` by SIGTERM, SIGINT and kill-server under
/// a `-C` and a `-CC` client, and checks the stop block and, with `hook`, that
/// every closed session ran the session-closed hook.
fn assert_control_stop_block(sessions: &[&str], hook: bool) -> Result<(), Box<dyn Error>> {
    for stop in ["-TERM", "-INT", "kill-server"] {
        for (mode, suffix) in [("-C", ""), ("-CC", rmux_proto::CONTROL_CONTROL_END)] {
            let harness = CliHarness::new("control-cli-server-shutdown")?;
            let mut daemon = harness.start_hidden_daemon()?;
            for session in sessions {
                assert_success(&harness.run(&["new-session", "-d", "-s", session, "sleep 30"])?);
            }
            let hook_log = harness.tmpdir().join("session-closed.log");
            if hook {
                let command = format!(
                    "run-shell 'echo #{{hook_session_name}} >> {}'",
                    hook_log.display()
                );
                assert_success(&harness.run(&["set-hook", "-g", "session-closed", &command])?);
            }
            let mut control =
                ControlProcess::spawn_in_mode(&harness, mode, &["attach-session", "-t", "alpha"])?;
            let attached = "%session-changed $0 alpha\n";
            control.wait_for_output(attached)?;

            if stop == "kill-server" {
                assert_success(&harness.run(&["kill-server"])?);
            } else {
                // The signal goes to the daemon this test started and still owns.
                let signalled = std::process::Command::new("kill")
                    .args([stop, &daemon.pid().to_string()])
                    .status()?;
                assert!(signalled.success(), "{stop}: kill {signalled:?}");
            }
            let output = control.wait_for_exit_with_open_stdin()?;
            daemon.child_mut().wait()?;

            let case = format!("{stop} {mode} {sessions:?} hook={hook}");
            assert_eq!(output.status.code(), Some(0), "{case}: {:?}", output.stdout);
            assert!(
                output.stderr.is_empty(),
                "{case}: stderr={:?}",
                output.stderr
            );
            let block = output
                .stdout
                .rsplit_once(attached)
                .map(|(_, block)| block)
                .unwrap_or_else(|| panic!("{case}: no attach record in {:?}", output.stdout));
            let expected = format!(
                "{}%exit\n{suffix}",
                "%sessions-changed\n".repeat(sessions.len())
            );
            assert_eq!(block, expected, "{case}");
            if hook {
                let closed = std::fs::read_to_string(&hook_log).unwrap_or_default();
                let mut closed: Vec<&str> = closed.lines().collect();
                closed.sort_unstable();
                assert_eq!(closed, sessions, "{case}: session-closed hooks");
            }
        }
    }
    Ok(())
}

#[test]
fn server_stopping_under_a_hook_that_calls_the_cli_exits_at_once_with_the_hook_complete(
) -> Result<(), Box<dyn Error>> {
    // tmux 3.7b closes every connection it accepts once it is exiting
    // (`server_accept`), so a session-closed hook that calls the CLI gets
    // its answer at once, runs to its end, and the server dies about 100 ms
    // after SIGTERM, SIGINT or kill-server. A hook left waiting would hold
    // the stop for the whole 5 s lifecycle grace.
    const STOPPED_WELL_BEFORE_THE_HOOK_GRACE: Duration = Duration::from_millis(2500);
    for stop in ["-TERM", "-INT", "kill-server"] {
        for (mode, suffix) in [("-C", ""), ("-CC", rmux_proto::CONTROL_CONTROL_END)] {
            let harness = CliHarness::new("control-cli-stop-hook-cli")?;
            let mut daemon = harness.start_hidden_daemon()?;
            assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
            let hook_log = harness.tmpdir().join("session-closed.log");
            let hook_script = harness.tmpdir().join("session-closed.sh");
            std::fs::write(
                &hook_script,
                format!(
                    "'{}' -S '{}' list-sessions -F x >/dev/null 2>&1\n\
                     echo \"cli-rc=$?\" >> '{log}'\n\
                     echo \"closed-$1\" >> '{log}'\n",
                    harness.base_command().get_program().to_string_lossy(),
                    harness.socket_path().display(),
                    log = hook_log.display(),
                ),
            )?;
            let hook = format!(
                "run-shell 'sh {} #{{hook_session_name}}'",
                hook_script.display()
            );
            assert_success(&harness.run(&["set-hook", "-g", "session-closed", &hook])?);
            let mut control =
                ControlProcess::spawn_in_mode(&harness, mode, &["attach-session", "-t", "alpha"])?;
            let attached = "%session-changed $0 alpha\n";
            control.wait_for_output(attached)?;

            let stopped_at = Instant::now();
            if stop == "kill-server" {
                assert_success(&harness.run(&["kill-server"])?);
            } else {
                // The signal goes to the daemon this test started and still owns.
                let signalled = std::process::Command::new("kill")
                    .args([stop, &daemon.pid().to_string()])
                    .status()?;
                assert!(signalled.success(), "{stop}: kill {signalled:?}");
            }
            daemon.child_mut().wait()?;
            let server_lifetime = stopped_at.elapsed();
            let output = control.wait_for_exit_with_open_stdin()?;

            let case = format!("{stop} {mode}");
            assert!(
                server_lifetime < STOPPED_WELL_BEFORE_THE_HOOK_GRACE,
                "{case}: the server lived {server_lifetime:?} after the stop"
            );
            let hook_lines = std::fs::read_to_string(&hook_log).unwrap_or_default();
            let hook_lines: Vec<&str> = hook_lines.lines().collect();
            assert_eq!(
                hook_lines.len(),
                2,
                "{case}: hook ran to its end: {hook_lines:?}"
            );
            assert!(
                hook_lines[0].starts_with("cli-rc=") && hook_lines[0] != "cli-rc=0",
                "{case}: the CLI call of a stopping server fails: {hook_lines:?}"
            );
            assert_eq!(hook_lines[1], "closed-alpha", "{case}: {hook_lines:?}");
            assert_eq!(output.status.code(), Some(0), "{case}: {:?}", output.stdout);
            let block = output
                .stdout
                .rsplit_once(attached)
                .map(|(_, block)| block)
                .unwrap_or_else(|| panic!("{case}: no attach record in {:?}", output.stdout));
            assert_eq!(
                block,
                format!("%sessions-changed\n%exit\n{suffix}"),
                "{case}"
            );
        }
    }
    Ok(())
}

struct ControlProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: SharedOutput,
    stdout_collector: Option<OutputCollector>,
    stderr_collector: Option<OutputCollector>,
}

impl ControlProcess {
    fn spawn(harness: &CliHarness, command_args: &[&str]) -> Result<Self, Box<dyn Error>> {
        Self::spawn_in_mode(harness, "-C", command_args)
    }

    fn spawn_in_mode(
        harness: &CliHarness,
        mode: &str,
        command_args: &[&str],
    ) -> Result<Self, Box<dyn Error>> {
        let mut command = harness.base_command();
        command
            .arg(mode)
            .args(command_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("control stdin is piped");
        let stdout = child.stdout.take().expect("control stdout is piped");
        let stderr = child.stderr.take().expect("control stderr is piped");
        let (stdout, stdout_collector) = spawn_output_collector(stdout);
        let (_stderr, stderr_collector) = spawn_output_collector(stderr);

        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            stdout,
            stdout_collector: Some(stdout_collector),
            stderr_collector: Some(stderr_collector),
        })
    }

    fn write_input(&mut self, input: &str) -> Result<(), Box<dyn Error>> {
        let stdin = self.stdin.as_mut().expect("control stdin remains open");
        stdin.write_all(input.as_bytes())?;
        stdin.flush()?;
        Ok(())
    }

    fn wait_for_output(&self, expected: &str) -> Result<(), Box<dyn Error>> {
        self.wait_for_occurrences(expected, 1)
    }

    fn stdout_text(&self) -> String {
        let bytes = self.stdout.lock().expect("control output lock");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn wait_for_occurrences(&self, expected: &str, count: usize) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while Instant::now() < deadline {
            if self.stdout_text().matches(expected).count() >= count {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = self.stdout_text();
        Err(format!("timed out waiting for {expected:?} in {output:?}").into())
    }

    fn assert_running(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(status) = self.child_mut().try_wait()? {
            return Err(format!("control client exited unexpectedly with {status}").into());
        }
        Ok(())
    }

    fn wait_for_exit_with_open_stdin(&mut self) -> Result<ControlOutput, Box<dyn Error>> {
        let status = self.wait_for_exit()?;
        self.collect_output(status)
    }

    fn close_stdin_and_wait(&mut self) -> Result<ControlOutput, Box<dyn Error>> {
        drop(self.stdin.take());
        let status = self.wait_for_exit()?;
        self.collect_output(status)
    }

    fn wait_for_exit(&mut self) -> Result<ExitStatus, Box<dyn Error>> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(status) = self.child_mut().try_wait()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!(
            "timed out waiting for control client process {}",
            self.child_mut().id()
        )
        .into())
    }

    fn collect_output(&mut self, status: ExitStatus) -> Result<ControlOutput, Box<dyn Error>> {
        drop(self.stdin.take());
        drop(self.child.take());
        let stdout = join_output_collector(&mut self.stdout_collector, "stdout")?;
        let stderr = join_output_collector(&mut self.stderr_collector, "stderr")?;
        Ok(ControlOutput {
            status,
            stdout: String::from_utf8(stdout)?,
            stderr: String::from_utf8(stderr)?,
        })
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("control child remains owned")
    }
}

impl Drop for ControlProcess {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if let Some(child) = self.child.as_mut() {
            let _ = terminate_child(child);
        }
        drop(self.child.take());
        let _ = join_output_collector(&mut self.stdout_collector, "stdout");
        let _ = join_output_collector(&mut self.stderr_collector, "stderr");
    }
}

struct ControlOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn spawn_output_collector<R>(mut reader: R) -> (SharedOutput, OutputCollector)
where
    R: Read + Send + 'static,
{
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mirror = Arc::clone(&shared);
    let handle = thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return Ok(output),
                Ok(count) => {
                    output.extend_from_slice(&buffer[..count]);
                    mirror
                        .lock()
                        .expect("control output mirror lock")
                        .extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    });
    (shared, handle)
}

fn join_output_collector(
    collector: &mut Option<OutputCollector>,
    label: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    match collector.take() {
        Some(collector) => collector
            .join()
            .map_err(|_| format!("{label} collector thread panicked"))?
            .map_err(Into::into),
        None => Ok(Vec::new()),
    }
}
