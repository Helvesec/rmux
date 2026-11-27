//! A popup job takes its target session's environment and default shell, as tmux does.
//!
//! Measured on tmux 3.7b with client B attached to its own session and `-t` naming the pane of
//! session A: the job sees A's session environment (not B's, not the requesting client's) and
//! runs under A's `default-shell`; with the targets swapped it sees B's. The server's own start
//! environment still reaches the job. Without `-t`, the target is the client's own pane.

use std::path::{Path, PathBuf};

use rmux_proto::{NewSessionRequest, Request, SessionName, TerminalSize};

use super::super::RequestHandler;
use super::completion_tests::{cli_command, exit_status, release_path, scene, ATTACH_PID};

/// A test-owned shell: it records which session's `default-shell` ran the job, then runs the
/// command with `/bin/sh`.
fn witness_shell(label: &str, shell_log: &Path) -> PathBuf {
    let path = release_path(label);
    crate::test_env::executable_fixture::write_executable(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s' '{label}' > '{}'\nexec /bin/sh \"$@\"\n",
            shell_log.display()
        ),
        0o755,
    )
    .expect("write a witness shell");
    path
}

async fn run(handler: &RequestHandler, command: String) {
    let response = handler.handle(cli_command(command)).await;
    assert_eq!(exit_status(&response), None, "{response:?}");
}

/// Runs a popup whose job writes `$RJE_SESSION`; returns it with the shell that ran the job.
async fn popup_job_context(
    handler: &RequestHandler,
    target: &str,
    shell_log: &Path,
    label: &str,
) -> (String, String) {
    let marker = release_path(label);
    let _ = std::fs::remove_file(shell_log);
    run(
        handler,
        format!(
            "display-popup -c {ATTACH_PID} {target} -w 20 -h 5 -E \
             'printf %s \"$RJE_SESSION\" > {}'",
            marker.display()
        ),
    )
    .await;
    let session = std::fs::read_to_string(&marker).expect("the popup job wrote its session value");
    let shell = std::fs::read_to_string(shell_log).expect("a session default-shell ran the job");
    let _ = std::fs::remove_file(&marker);
    (session, shell)
}

#[tokio::test]
async fn a_popup_job_takes_its_target_session_environment_and_shell() {
    let (handler, _control_rx) = scene("ct-env-client").await;
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: SessionName::new("ct-env-target").expect("valid session name"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let shell_log = release_path("ct-env-shell-log");
    let client_shell = witness_shell("client-shell", &shell_log);
    let target_shell = witness_shell("target-shell", &shell_log);
    for (session, value, shell) in [
        ("ct-env-client", "client-session", &client_shell),
        ("ct-env-target", "target-session", &target_shell),
    ] {
        run(
            &handler,
            format!("set-environment -t {session} RJE_SESSION {value}"),
        )
        .await;
        run(
            &handler,
            format!("set-option -t {session} default-shell {}", shell.display()),
        )
        .await;
    }

    assert_eq!(
        popup_job_context(
            &handler,
            "-t ct-env-target:0.0",
            &shell_log,
            "ct-env-explicit"
        )
        .await,
        ("target-session".to_owned(), "target-shell".to_owned()),
        "an explicit target pane gives the job its session's environment and shell"
    );
    assert_eq!(
        popup_job_context(&handler, "", &shell_log, "ct-env-own").await,
        ("client-session".to_owned(), "client-shell".to_owned()),
        "without -t the client's own pane is the target"
    );
    for path in [&shell_log, &client_shell, &target_shell] {
        let _ = std::fs::remove_file(path);
    }
}

/// Measured on tmux 3.7b: without a shell command, the popup runs the target session's
/// `default-command` (client B / target A runs A's), and the client's own one without `-t`.
#[tokio::test]
async fn a_popup_without_a_command_runs_its_target_session_default_command() {
    let (handler, _control_rx) = scene("ct-dc-client").await;
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: SessionName::new("ct-dc-target").expect("valid session name"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let marker = release_path("ct-dc-marker");
    for (session, label) in [
        ("ct-dc-client", "client-command"),
        ("ct-dc-target", "target-command"),
    ] {
        run(
            &handler,
            format!(
                "set-option -t {session} default-command 'printf %s {label} > {}'",
                marker.display()
            ),
        )
        .await;
    }

    for (target, expected) in [
        ("-t ct-dc-target:0.0", "target-command"),
        ("", "client-command"),
    ] {
        let _ = std::fs::remove_file(&marker);
        run(
            &handler,
            format!("display-popup -c {ATTACH_PID} {target} -w 20 -h 5 -E"),
        )
        .await;
        let ran = std::fs::read_to_string(&marker).expect("a default-command ran");
        assert_eq!(ran, expected, "display-popup {target} without a command");
    }
    let _ = std::fs::remove_file(&marker);
}
