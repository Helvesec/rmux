//! D1: a `display-popup` command completes only when its popup leaves the client.
//!
//! The contract was measured on tmux 3.7b with a real client and real popup jobs: the waiting
//! command reports the job's exit status when the job ended first (exit 3 gives 3) and 129 when
//! the popup was freed while its job still ran (`display-popup -C`). A binding's popup holds that
//! binding's next command until it closes, while the client's input keeps reaching the popup; a
//! command sent from outside waits the same way even while one of the client's bindings runs.
//! The request below is the one the CLI sends for a queued command.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::{
    NewSessionRequest, Request, Response, SessionName, SourceFileRequest, TerminalSize,
    INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH,
};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Duration};

use super::super::attach_support::on_attached_input_path;
use super::super::RequestHandler;

pub(super) const ATTACH_PID: u32 = 974_101;

pub(super) fn cli_command(command: String) -> Request {
    Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some(command),
    }))
}

/// One session with one registered client of a real size, so a popup fits on it.
pub(super) async fn scene(
    name: &str,
) -> (
    RequestHandler,
    mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let handler = RequestHandler::new();
    let session = SessionName::new(name).expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(ATTACH_PID, session, control_tx)
        .await;
    handler
        .handle_attached_resize(ATTACH_PID, TerminalSize { cols: 80, rows: 24 })
        .await
        .expect("the client takes a real size");
    (handler, control_rx)
}

pub(super) fn release_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("rmux-d1-{label}-{}-{nanos}", std::process::id()))
}

pub(super) fn exit_status(response: &Response) -> Option<i32> {
    match response {
        Response::SourceFile(source) => source.exit_status(),
        other => panic!("expected the queued command response, got {other:?}"),
    }
}

pub(super) async fn wait_for_popup(handler: &RequestHandler) {
    timeout(Duration::from_secs(5), async {
        while !handler.overlay_active(ATTACH_PID).await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the popup opens on the client");
}

#[tokio::test]
async fn a_cli_popup_completes_when_its_job_exits_and_reports_its_status() {
    let (handler, _control_rx) = scene("d1-job-exit").await;
    let release = release_path("exit");
    let job = format!(
        "sh -c 'while [ ! -e {} ]; do sleep 0.02; done; exit 3'",
        release.display()
    );
    let command = format!("display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"{job}\"");
    let waiting = tokio::spawn({
        let handler = handler.clone();
        async move { handler.handle(cli_command(command)).await }
    });

    wait_for_popup(&handler).await;
    // The job cannot exit before the release file exists, so a finished command here would be
    // the defect: the CLI freed while its popup job still waits.
    sleep(Duration::from_millis(300)).await;
    assert!(
        !waiting.is_finished(),
        "display-popup must not complete while its job is still running"
    );

    std::fs::write(&release, b"go").expect("release the popup job");
    let response = timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the command completes once the job exits")
        .expect("the command task");
    let _ = std::fs::remove_file(&release);
    assert_eq!(
        exit_status(&response),
        Some(3),
        "the job's exit status is the command's status, as tmux 3.7b reports it"
    );
    assert!(
        !handler.overlay_active(ATTACH_PID).await,
        "-E closed the popup before the command completed"
    );
}

#[tokio::test]
async fn a_cli_popup_closed_by_display_popup_c_reports_129() {
    let (handler, _control_rx) = scene("d1-close").await;
    let command = format!("display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"sleep 30\"");
    let waiting = tokio::spawn({
        let handler = handler.clone();
        async move { handler.handle(cli_command(command)).await }
    });

    wait_for_popup(&handler).await;
    sleep(Duration::from_millis(100)).await;
    assert!(
        !waiting.is_finished(),
        "the command waits on the open popup"
    );

    let closed = handler
        .handle(cli_command(format!("display-popup -C -c {ATTACH_PID}")))
        .await;
    assert_eq!(
        exit_status(&closed),
        None,
        "the closing command itself succeeds"
    );
    let response = timeout(Duration::from_secs(10), waiting)
        .await
        .expect("closing the popup completes the waiting command")
        .expect("the command task");
    assert_eq!(
        exit_status(&response),
        Some(129),
        "a popup freed while its job runs reports 128 + SIGHUP, as tmux 3.7b does"
    );
}

pub(super) fn blocking_job(release: &Path, status: i32) -> String {
    format!(
        "sh -c 'while [ ! -e {} ]; do sleep 0.02; done; exit {status}'",
        release.display()
    )
}

pub(super) async fn bind_root_key(handler: &RequestHandler, key: &str, commands: &str) {
    let bound = handler
        .handle(cli_command(format!("bind-key -n {key} {{ {commands} }}")))
        .await;
    assert_eq!(exit_status(&bound), None, "{bound:?}");
}

pub(super) async fn wait_for_file(path: &Path) {
    timeout(Duration::from_secs(10), async {
        while !path.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the file appears");
}

pub(super) const F10: &[u8] = b"\x1b[21~";
const F12: &[u8] = b"\x1b[24~";

#[tokio::test]
async fn a_binding_popup_frees_its_client_input_and_its_next_command_runs_after_it_closes() {
    let (handler, _control_rx) = scene("d1-binding-chain").await;
    let release = release_path("binding-job");
    let consumer = release_path("binding-consumer");
    bind_root_key(
        &handler,
        "F12",
        &format!(
            "display-popup -w 20 -h 5 -E \"{}\" ; run-shell \"touch {}\"",
            blocking_job(&release, 0),
            consumer.display()
        ),
    )
    .await;

    timeout(
        Duration::from_secs(5),
        handler.handle_attached_live_input_for_test(ATTACH_PID, F12),
    )
    .await
    .expect("the client's input path is free while the binding's popup stays open")
    .expect("the key is handled");
    wait_for_popup(&handler).await;
    sleep(Duration::from_millis(300)).await;
    assert!(
        !consumer.exists(),
        "the binding's next command must wait until its popup closes"
    );

    std::fs::write(&release, b"go").expect("release the popup job");
    wait_for_file(&consumer).await;
    assert!(
        !handler.overlay_active(ATTACH_PID).await,
        "the popup closed before the binding's next command ran"
    );
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&consumer);
}

#[tokio::test]
async fn a_cli_popup_during_a_distinct_binding_frees_the_input_and_waits_for_its_job() {
    let (handler, _control_rx) = scene("d1-distinct-binding").await;
    let binding_release = release_path("distinct-binding");
    bind_root_key(
        &handler,
        "F10",
        &format!(
            "run-shell \"while [ ! -e {} ]; do sleep 0.02; done\"",
            binding_release.display()
        ),
    )
    .await;
    let input = tokio::spawn({
        let handler = handler.clone();
        async move {
            handler
                .handle_attached_live_input_for_test(ATTACH_PID, F10)
                .await
        }
    });
    sleep(Duration::from_millis(200)).await;
    assert!(
        !input.is_finished(),
        "the client's input path awaits its running binding"
    );

    let release = release_path("distinct-job");
    let command = format!(
        "display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"{}\"",
        blocking_job(&release, 5)
    );
    let waiting = tokio::spawn({
        let handler = handler.clone();
        async move { handler.handle(cli_command(command)).await }
    });
    wait_for_popup(&handler).await;
    timeout(Duration::from_secs(5), input)
        .await
        .expect("the popup frees the input path the binding held")
        .expect("the input task")
        .expect("the key is handled");
    sleep(Duration::from_millis(300)).await;
    assert!(
        !waiting.is_finished(),
        "a running binding of the client is no reason for this command to stop waiting"
    );

    std::fs::write(&release, b"go").expect("release the popup job");
    let response = timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the command completes once the job exits")
        .expect("the command task");
    assert_eq!(exit_status(&response), Some(5));
    std::fs::write(&binding_release, b"go").expect("end the binding");
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&binding_release);
}

#[tokio::test]
async fn a_popup_its_attached_client_runs_outside_its_input_path_waits_for_its_job() {
    let (handler, _control_rx) = scene("d1-attached").await;
    let release = release_path("attached-job");
    let consumer = release_path("attached-consumer");
    let command = format!(
        "display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"{}\" ; run-shell \"touch {}\"",
        blocking_job(&release, 0),
        consumer.display()
    );
    let waiting = tokio::spawn({
        let handler = handler.clone();
        async move {
            handler
                .dispatch(ATTACH_PID, cli_command(command))
                .await
                .response
        }
    });
    wait_for_popup(&handler).await;
    sleep(Duration::from_millis(300)).await;
    assert!(
        !waiting.is_finished(),
        "an attached requester is no reason to stop waiting"
    );
    assert!(!consumer.exists(), "the next command waits for the popup");

    std::fs::write(&release, b"go").expect("release the popup job");
    timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the commands complete once the job exits")
        .expect("the command task");
    assert!(
        consumer.exists(),
        "the next command ran after the popup closed"
    );
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&consumer);
}

#[tokio::test]
async fn a_popup_shown_inline_on_its_own_client_input_path_does_not_wait() {
    let (handler, _control_rx) = scene("d1-inline").await;
    let command = format!("display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"sleep 30\"");
    let outcome = timeout(
        Duration::from_secs(5),
        on_attached_input_path(
            ATTACH_PID,
            handler.dispatch(ATTACH_PID, cli_command(command)),
        ),
    )
    .await
    .expect("waiting there would keep the popup from ever receiving its input");
    assert_eq!(exit_status(&outcome.response), None);
    assert!(
        handler.overlay_active(ATTACH_PID).await,
        "the popup is open"
    );

    handler
        .clear_interactive_overlay(ATTACH_PID, true)
        .await
        .expect("close the popup");
}
