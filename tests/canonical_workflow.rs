#![cfg(unix)]

mod common;

use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::process::Output;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use common::{
    assert_only_default_socket, assert_success, child_process_states, read_until_contains,
    read_until_contains_all, stderr, stdout, verify_fixture_coherence, AttachedSession, CliHarness,
    BINARY_OVERRIDE_ENV, CANONICAL_SESSION_WORKFLOW, WORKFLOW_COLORTERM_PRINT_COMMAND,
};
use rmux_pty::TerminalSize;

const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const NONBLOCKING_ATTACH_TIMEOUT: Duration = Duration::from_millis(500);
const CHURN_ITERATIONS: usize = 8;
const REAP_TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn workflow_fixture_is_separately_verified() {
    verify_fixture_coherence();
}

#[test]
fn canonical_session_workflow_runs_end_to_end() -> Result<(), Box<dyn Error>> {
    let _guard = workflow_integration_guard();
    let harness = CliHarness::new("canonical-workflow")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let hook_path = harness.tmpdir().join("client-attached.hook");
    let hook_command = format!("sleep 1; printf attached > '{}'", hook_path.display());
    let mut attached_session = None;

    for step in CANONICAL_SESSION_WORKFLOW {
        match step.label {
            "cleanup" => {
                let output = harness.run(step.argv)?;
                assert_cleanup_kill_result(&output);
            }
            "new-session" => {
                let output = harness.run_with(step.argv, |command| {
                    command.env(
                        BINARY_OVERRIDE_ENV,
                        harness.tmpdir().join("rmux-launcher.sh"),
                    );
                })?;
                assert_success(&output);
            }
            "client-attached-hook" => {
                let args = [
                    "set-hook",
                    "-t",
                    "workflow",
                    "client-attached",
                    hook_command.as_str(),
                ];
                assert_success(&harness.run(&args)?);
            }
            "attach-session" => {
                assert!(
                    !hook_path.exists(),
                    "hook output must not exist before attach"
                );

                let mut attach =
                    AttachedSession::spawn(&harness, "workflow", TerminalSize::new(200, 50))?;
                attach.wait_for_raw_mode(NONBLOCKING_ATTACH_TIMEOUT)?;
                assert!(
                    !hook_path.exists(),
                    "attach must not block on the sleeping client-attached hook"
                );

                let _frame = read_until_contains_all(
                    attach.master_mut(),
                    &[
                        "\u{1b}[1;35H",
                        "\u{1b}[17;35H├",
                        "\u{1b}[34;35H├",
                        "\u{1b}[50;35H",
                        "│",
                        "\u{1b}[0m\x1b[u",
                    ],
                    ATTACH_TIMEOUT,
                )?;

                attach.master_mut().write_all(b"attach-stream\r")?;
                let echoed =
                    read_until_contains(attach.master_mut(), "attach-stream", ATTACH_TIMEOUT)?;
                assert!(
                    echoed.contains("attach-stream"),
                    "attach should surface client keystrokes through the pane tty echo"
                );

                attached_session = Some(attach);
            }
            "send-keys-env" => {
                assert_success(&harness.run(step.argv)?);
                let attach = attached_session
                    .as_mut()
                    .expect("attach-session step must complete before send-keys-env");
                let output = read_until_contains(attach.master_mut(), "truecolor", ATTACH_TIMEOUT)?;
                assert!(
                    output.contains(WORKFLOW_COLORTERM_PRINT_COMMAND)
                        && output.contains("truecolor"),
                    "send-keys-env should surface the pane's inherited COLORTERM value"
                );
            }
            "send-keys-sleep" => {
                assert_success(&harness.run(step.argv)?);
                let attach = attached_session
                    .as_mut()
                    .expect("attach-session step must complete before send-keys-sleep");
                let output = read_until_contains(attach.master_mut(), "sleep 5", ATTACH_TIMEOUT)?;
                assert!(
                    output.contains("sleep 5"),
                    "send-keys-sleep should start a long-running command in the attached pane"
                );
            }
            "send-keys-ctrl-c" => {
                let output = harness.run(step.argv)?;
                assert_success(&output);
                let attach = attached_session
                    .as_mut()
                    .expect("attach-session step must complete before send-keys-ctrl-c");
                attach
                    .master_mut()
                    .write_all(b"printf ctrl-c-recovered\r")?;
                let output =
                    read_until_contains(attach.master_mut(), "ctrl-c-recovered", ATTACH_TIMEOUT)?;
                assert!(
                    output.contains("ctrl-c-recovered"),
                    "send-keys-ctrl-c should interrupt the pane command so the shell recovers"
                );
            }
            "detach-client" => {
                assert_success(&harness.run(step.argv)?);

                let attach = attached_session
                    .as_mut()
                    .expect("attach-session step must complete before detach-client");
                let status = attach.wait_for_exit(ATTACH_TIMEOUT)?;
                assert_eq!(status.code(), Some(0));
                attach.assert_restored()?;
                wait_for_path(&hook_path, ATTACH_TIMEOUT)?;
            }
            "has-session-after-kill" => {
                let output = harness.run(step.argv)?;
                assert_absent_session(&output);
            }
            _ => {
                assert!(
                    !step.runtime_argv,
                    "step '{}' requires runtime argv but fell through to the default handler",
                    step.label
                );
                let output = harness.run(step.argv)?;
                assert_success(&output);
            }
        }
    }

    Ok(())
}

#[test]
fn multiple_sessions_remain_isolated_after_targeted_kill() -> Result<(), Box<dyn Error>> {
    let _guard = workflow_integration_guard();
    let harness = CliHarness::new("session-isolation")?;
    let _daemon = harness.start_hidden_daemon()?;

    for session in ["alpha", "beta"] {
        let output = harness.run(&["new-session", "-d", "-s", session])?;
        assert_success(&output);
    }

    assert_success(&harness.run(&["has-session", "-t", "alpha"])?);
    assert_success(&harness.run(&["has-session", "-t", "beta"])?);
    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);

    let alpha_missing = harness.run(&["has-session", "-t", "alpha"])?;
    assert_missing_has_session(&alpha_missing, "alpha");

    assert_success(&harness.run(&["has-session", "-t", "beta"])?);

    let repeated_kill = harness.run(&["kill-session", "-t", "alpha"])?;
    assert_missing_kill(&repeated_kill, "alpha");
    assert_success(&harness.run(&["has-session", "-t", "beta"])?);

    Ok(())
}

#[test]
fn rapid_create_kill_churn_reaps_children_without_socket_leaks() -> Result<(), Box<dyn Error>> {
    let _guard = workflow_integration_guard();
    let harness = CliHarness::new("rapid-churn")?;
    let daemon = harness.start_hidden_daemon()?;
    run_success_with_transient_retry(&harness, &["new-session", "-d", "-s", "keepalive"])?;
    let baseline_states = child_process_states(daemon.pid())?;

    for iteration in 0..CHURN_ITERATIONS {
        let session_name = format!("churn{iteration}");
        run_success_with_transient_retry(
            &harness,
            &["new-session", "-d", "-s", session_name.as_str()],
        )?;

        run_success_with_transient_retry(&harness, &["has-session", "-t", session_name.as_str()])?;
        std::thread::sleep(Duration::from_millis(25));
        run_success_with_transient_retry(&harness, &["kill-session", "-t", session_name.as_str()])?;

        // Verify tmux compatibility: killing the same session again reports a missing target.
        let repeated_kill = harness.run(&["kill-session", "-t", session_name.as_str()])?;
        assert_missing_kill(&repeated_kill, &session_name);

        let absent = harness.run(&["has-session", "-t", session_name.as_str()])?;
        assert_missing_has_session(&absent, &session_name);
        wait_for_child_process_baseline(daemon.pid(), &baseline_states, REAP_TIMEOUT)?;
        assert_only_default_socket(harness.socket_path())?;
    }

    Ok(())
}

fn run_success_with_transient_retry(
    harness: &CliHarness,
    args: &[&str],
) -> Result<(), Box<dyn Error>> {
    let mut last_output = None;

    for _ in 0..3 {
        let output = harness.run(args)?;
        if output.status.code() == Some(0)
            && stdout(&output).is_empty()
            && stderr(&output).is_empty()
        {
            return Ok(());
        }

        let retryable = stderr(&output).contains("Resource temporarily unavailable");
        last_output = Some(output);
        if !retryable {
            break;
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    let output = last_output.as_ref().expect("at least one command attempt");
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected successful command {:?}, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        args,
        output.status,
        stdout(output),
        stderr(output)
    );
    assert!(stdout(output).is_empty(), "stdout should be empty");
    assert!(stderr(output).is_empty(), "stderr should be empty");
    Ok(())
}

fn assert_absent_session(output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(output).is_empty(),
        "absent has-session should not produce stdout"
    );
    assert!(
        stderr(output).contains("no server running on "),
        "absent has-session should report absent server, got: {}",
        stderr(output)
    );
}

fn assert_missing_has_session(output: &Output, session_name: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(output).is_empty(),
        "missing has-session should not produce stdout"
    );
    assert!(
        stderr(output).contains(&format!("can't find session: {session_name}")),
        "missing has-session should report the missing session, got: {}",
        stderr(output)
    );
}

fn assert_cleanup_kill_result(output: &Output) {
    match output.status.code() {
        Some(0) => {
            assert!(
                stdout(output).is_empty(),
                "cleanup kill should not produce stdout"
            );
            assert!(
                stderr(output).is_empty(),
                "cleanup kill should not produce stderr"
            );
        }
        Some(1) => {
            assert!(
                stdout(output).is_empty(),
                "cleanup kill should not produce stdout"
            );
            let stderr_text = stderr(output);
            assert!(
                stderr_text.contains("no server running on ")
                    || stderr_text.contains("session not found: workflow"),
                "cleanup kill should report an absent server or a missing session, got: {stderr_text}",
            );
        }
        other => panic!(
            "cleanup kill should exit 0 or 1, got {:?}\nstdout:\n{}\nstderr:\n{}",
            other,
            stdout(output),
            stderr(output)
        ),
    }
}

fn assert_missing_kill(output: &Output, session_name: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(output).is_empty(),
        "missing kill should not produce stdout"
    );
    assert!(
        stderr(output).contains(&format!("session not found: {session_name}")),
        "missing kill should report the missing session, got: {}",
        stderr(output)
    );
}

fn workflow_integration_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_child_process_baseline(
    parent_pid: u32,
    expected_states: &[String],
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut expected = expected_states
        .iter()
        .map(|state| normalize_child_process_state(state))
        .collect::<Vec<_>>();
    expected.sort();

    while Instant::now() < deadline {
        let states = child_process_states(parent_pid)?;
        let mut normalized_states = states
            .iter()
            .map(|state| normalize_child_process_state(state))
            .collect::<Vec<_>>();
        normalized_states.sort();
        if normalized_states == expected {
            return Ok(());
        }

        assert!(
            states.iter().all(|state| !state.starts_with('Z')),
            "zombie child processes remained under daemon {parent_pid}: {states:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let states = child_process_states(parent_pid)?;
    let mut normalized_states = states
        .iter()
        .map(|state| normalize_child_process_state(state))
        .collect::<Vec<_>>();
    normalized_states.sort();
    Err(format!(
        "timed out waiting for daemon {parent_pid} to return to child states {expected:?}: raw={states:?} normalized={normalized_states:?}"
    )
    .into())
}

fn normalize_child_process_state(state: &str) -> String {
    let mut chars = state.chars();
    match chars.next() {
        // The direct child under the daemon can legitimately oscillate between
        // runnable and interruptible sleep while remaining the same live process.
        Some('R' | 'S') => format!("A{}", chars.collect::<String>()),
        Some(other) => format!("{}{}", other, chars.collect::<String>()),
        None => String::new(),
    }
}

fn wait_for_path(path: &Path, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    Err(format!("timed out waiting for '{}'", path.display()).into())
}
