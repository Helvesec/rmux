//! A popup job starts in its target session's directory, as tmux does.
//!
//! Measured on tmux 3.7b with client B attached to its own session and `-t` naming the pane of
//! session A: the job runs in A's session directory, neither B's nor the server's. Without
//! `-t`, the target is the client's own pane, so the job runs in the client session's directory.

use std::path::{Path, PathBuf};

use rmux_proto::{NewSessionRequest, Request, SessionName, TerminalSize};

use super::super::RequestHandler;
use super::completion_tests::{cli_command, exit_status, release_path, scene, ATTACH_PID};

fn directory(label: &str) -> PathBuf {
    let path = release_path(label);
    std::fs::create_dir(&path).expect("create a session directory");
    path.canonicalize().expect("canonical session directory")
}

async fn set_session_directory(handler: &RequestHandler, name: &str, path: &Path) {
    let session = SessionName::new(name).expect("valid session name");
    let mut state = handler.state.lock().await;
    state
        .sessions
        .set_cwd(&session, Some(path.to_path_buf()))
        .expect("the session exists");
}

/// Runs a popup whose job writes its physical working directory, and returns it.
async fn popup_job_directory(handler: &RequestHandler, target: &str, label: &str) -> PathBuf {
    let marker = release_path(label);
    let response = handler
        .handle(cli_command(format!(
            "display-popup -c {ATTACH_PID} {target} -w 20 -h 5 -E \"pwd -P > {}\"",
            marker.display()
        )))
        .await;
    assert_eq!(exit_status(&response), None, "{response:?}");
    let written = std::fs::read_to_string(&marker).expect("the popup job wrote its directory");
    let _ = std::fs::remove_file(&marker);
    PathBuf::from(written.trim_end())
}

#[tokio::test]
async fn a_popup_job_starts_in_its_target_session_directory() {
    let (handler, _control_rx) = scene("ct-client").await;
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: SessionName::new("ct-target").expect("valid session name"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let client_directory = directory("ct-client-dir");
    let target_directory = directory("ct-target-dir");
    set_session_directory(&handler, "ct-client", &client_directory).await;
    set_session_directory(&handler, "ct-target", &target_directory).await;

    assert_eq!(
        popup_job_directory(&handler, "-t ct-target:0.0", "ct-explicit-target").await,
        target_directory,
        "an explicit target pane gives the job its session's directory"
    );
    assert_eq!(
        popup_job_directory(&handler, "", "ct-client-target").await,
        client_directory,
        "without -t the client's own pane is the target"
    );
    let _ = std::fs::remove_dir(&client_directory);
    let _ = std::fs::remove_dir(&target_directory);
}
