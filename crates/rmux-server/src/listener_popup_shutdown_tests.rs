//! D1: a CLI `display-popup` still waiting for its popup when the server shuts down.
//!
//! Measured on tmux 3.7b with a real client: killing the server under a waiting CLI popup
//! completes the CLI with status 0, keeps what the request printed before the popup and runs
//! nothing it queued after the popup. The request goes through the real connection loop here,
//! because only that loop can lose or write the answer.

use std::path::{Path, PathBuf};

use super::connection_test_support::{
    connected_streams, finish_connection, read_test_response, spawn_connection, write_test_request,
};
use super::*;

use rmux_proto::{
    NewSessionRequest, PaneTarget, SessionName, SourceFileRequest, SourceFileResponse,
    TerminalSize, INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH,
};

const ATTACH_PID: u32 = 974_301;

fn cli_command(command: String) -> Request {
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

fn scratch(label: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("rmux-d1-shutdown-{label}-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

type WaitingRequest = (
    Arc<RequestHandler>,
    LocalStream,
    watch::Sender<()>,
    tokio::task::JoinHandle<io::Result<()>>,
    tokio::sync::mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
);

/// A session with a real-size client and a connection sending `before`, then a popup whose job
/// signals it runs, then a consumer; returns once the job runs and the request still waits.
async fn waiting_popup_request(
    name: &str,
    before: &str,
    ready: &Path,
    consumer: &Path,
) -> io::Result<WaitingRequest> {
    let request = cli_command(format!(
        "{before}display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"sh -c 'touch {}; sleep 30'\" ; \
         run-shell \"touch {}\"",
        ready.display(),
        consumer.display()
    ));
    waiting_request(name, request, ready).await
}

/// Sends `request` on a real connection of a session with a real-size client and returns once
/// the popup job the request shows runs, while the request still waits.
async fn waiting_request(name: &str, request: Request, ready: &Path) -> io::Result<WaitingRequest> {
    let handler = Arc::new(RequestHandler::new());
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
    let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(ATTACH_PID, session, control_tx)
        .await;
    handler
        .handle_attached_resize(ATTACH_PID, TerminalSize { cols: 80, rows: 24 })
        .await
        .expect("the client takes a real size");

    let (server, mut client) = connected_streams(name).await?;
    let peer = PeerIdentity {
        pid: std::process::id(),
        uid: rmux_os::identity::real_user_id(),
        user: rmux_os::identity::UserIdentity::Uid(rmux_os::identity::real_user_id()),
    };
    let (shutdown_tx, connection) = spawn_connection(&handler, peer, server);
    write_test_request(&mut client, request).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the popup job runs on the client");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), read_test_response(&mut client))
            .await
            .is_err(),
        "the CLI waits for its popup"
    );
    Ok((handler, client, shutdown_tx, connection, control_rx))
}

async fn shutdown_response(client: &mut LocalStream) -> io::Result<SourceFileResponse> {
    let response = tokio::time::timeout(Duration::from_secs(5), read_test_response(client))
        .await
        .expect("the connection answers before it closes")?;
    let Response::SourceFile(response) = response else {
        panic!("expected the queued command response, got {response:?}");
    };
    Ok(response)
}

#[tokio::test]
async fn waiting_popup_helper_keeps_the_attach_channel_live_until_explicit_cleanup(
) -> io::Result<()> {
    let (ready, consumer) = (scratch("attach-ready"), scratch("attach-consumer"));
    let (handler, mut client, shutdown_tx, connection, control_rx) =
        waiting_popup_request("d1-shutdown-attach", "", &ready, &consumer).await?;

    // A fresh clipboard query must reach the attach channel after the helper returns,
    // while admission is open and the popup still waits. It cannot coalesce a refresh.
    let probe = handler
        .handle(cli_command(format!("refresh-client -t {ATTACH_PID} -l")))
        .await;

    handler.close_normal_request_admission();
    drop(shutdown_tx);
    let answer = shutdown_response(&mut client).await;
    let finished = finish_connection(connection).await;
    let _ = handler
        .handle(cli_command(format!("display-popup -C -c {ATTACH_PID}")))
        .await;
    drop(control_rx);
    let _ = std::fs::remove_file(&ready);
    let _ = std::fs::remove_file(&consumer);

    let Response::SourceFile(probe) = probe else {
        panic!("expected the attach channel probe response, got {probe:?}");
    };
    assert_eq!(
        probe.exit_status, None,
        "the attach channel stays live after the helper returns and before cleanup: {probe:?}"
    );
    answer?;
    finished?;
    Ok(())
}

#[tokio::test]
async fn shutdown_answers_a_waiting_cli_popup_with_status_zero_and_runs_nothing_after_it(
) -> io::Result<()> {
    let (ready, consumer) = (scratch("ready"), scratch("consumer"));
    let (handler, mut client, shutdown_tx, connection, control_rx) =
        waiting_popup_request("d1-shutdown", "", &ready, &consumer).await?;

    // The server starts closing, and its teardown frees the popup before this connection
    // observes the shutdown: the caller must neither complete with 129 nor go on to run-shell.
    handler.close_normal_request_admission();
    let closed = handler
        .handle(cli_command(format!("display-popup -C -c {ATTACH_PID}")))
        .await;
    assert!(matches!(closed, Response::SourceFile(_)), "{closed:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !consumer.exists(),
        "nothing queued after the popup may run once the server is closing"
    );

    drop(shutdown_tx);
    let response = shutdown_response(&mut client).await?;
    assert_eq!(
        (response.exit_status, response.output),
        (None, None),
        "no failure and nothing printed: the CLI exits 0, as tmux 3.7b does"
    );
    finish_connection(connection).await?;
    drop(control_rx);
    let _ = std::fs::remove_file(&ready);
    assert!(
        !consumer.exists(),
        "the abandoned queue never ran run-shell"
    );
    Ok(())
}

#[tokio::test]
async fn shutdown_keeps_what_the_request_printed_before_its_popup() -> io::Result<()> {
    let (ready, consumer) = (scratch("before-ready"), scratch("before-consumer"));
    let (handler, mut client, shutdown_tx, connection, control_rx) = waiting_popup_request(
        "d1-shutdown-before",
        "display-message -p BEFORE ; ",
        &ready,
        &consumer,
    )
    .await?;

    // The connection learns of the shutdown while the popup is still open and before the wait
    // sees the server closing: it must let the request finish rather than cancel it.
    drop(shutdown_tx);
    tokio::time::sleep(Duration::from_millis(100)).await;
    handler.close_normal_request_admission();
    let response = shutdown_response(&mut client).await?;
    let stdout = response
        .output
        .as_ref()
        .map(|output| String::from_utf8_lossy(output.stdout()).into_owned());
    assert_eq!(
        stdout.as_deref(),
        Some("BEFORE\n"),
        "the output acquired before the popup is part of the answer, as tmux 3.7b prints it"
    );
    assert_eq!(response.exit_status, None, "no failure: the CLI exits 0");
    finish_connection(connection).await?;
    // The server's teardown is not part of this test; close the popup and its job here.
    let _ = handler
        .handle(cli_command(format!("display-popup -C -c {ATTACH_PID}")))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(control_rx);
    let _ = std::fs::remove_file(&ready);
    assert!(
        !consumer.exists(),
        "nothing after the popup runs during the shutdown"
    );
    Ok(())
}

#[tokio::test]
async fn observed_connection_shutdown_stops_a_popup_before_admission_closes() -> io::Result<()> {
    let (ready, consumer) = (scratch("observed-ready"), scratch("observed-consumer"));
    let request = cli_command(format!(
        "display-message -p BEFORE ; \
         display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"sh -c 'touch {}; read hold'\" ; \
         display-message -p AFTER ; run-shell \"touch {}\"",
        ready.display(),
        consumer.display()
    ));
    let (handler, mut client, shutdown_tx, connection, control_rx) =
        waiting_request("d1-shutdown-observed", request, &ready).await?;

    // Freeze the order from the intermittent case without relying on its sleep:
    // the connection observes shutdown, then the popup completes while admission is open.
    drop(shutdown_tx);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !shutdown_answer::connection_shutdown_observed_for_test() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the connection observes shutdown while its popup waits");
    let closed = handler
        .handle(cli_command(format!("display-popup -C -c {ATTACH_PID}")))
        .await;
    assert!(matches!(closed, Response::SourceFile(_)), "{closed:?}");
    let response = shutdown_response(&mut client).await?;
    handler.close_normal_request_admission();
    finish_connection(connection).await?;
    drop(control_rx);
    let _ = std::fs::remove_file(&ready);

    let stdout = response.output.as_ref().map(|output| output.stdout());
    assert_eq!(
        stdout,
        Some(b"BEFORE\n".as_slice()),
        "nothing after the popup runs once its connection observed shutdown: {response:?}"
    );
    assert_eq!(
        response.exit_status, None,
        "shutdown keeps the earlier status"
    );
    assert!(
        !consumer.exists(),
        "the shutdown request never runs its consumer"
    );
    Ok(())
}

#[tokio::test]
async fn shutdown_stops_the_whole_source_file_request_before_its_next_file() -> io::Result<()> {
    let name = "d1-shutdown-files";
    let (ready, consumer) = (scratch("files-ready"), scratch("files-consumer"));
    let (first, second) = (scratch("files-first.conf"), scratch("files-second.conf"));
    std::fs::write(
        &first,
        format!(
            "select-window -t no-such-session-r4:9\ndisplay-message -p BEFORE\n\
             display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"sh -c 'touch {}; read shutdown_hold'\"\n",
            ready.display()
        ),
    )?;
    std::fs::write(
        &second,
        format!(
            "display-message -p AFTER\nrun-shell \"touch {}\"\n",
            consumer.display()
        ),
    )?;
    // One SourceFile request, two files, and an explicit target that stays valid.
    let request = Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec![first.display().to_string(), second.display().to_string()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: Some(PaneTarget::new(
            SessionName::new(name).expect("valid session name"),
            0,
        )),
        caller_cwd: None,
        stdin: None,
    }));
    let (handler, mut client, shutdown_tx, connection, control_rx) =
        waiting_request(name, request, &ready).await?;

    // Select the popup's completion while admission is still open, then hold that result
    // until shutdown starts. No scheduling delay can let the next file escape this order.
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    shutdown_answer::gate_popup_completion_for_test(Arc::clone(&gate));
    let closed = handler
        .handle(cli_command(format!("display-popup -C -c {ATTACH_PID}")))
        .await;
    assert!(matches!(closed, Response::SourceFile(_)), "{closed:?}");
    tokio::time::timeout(Duration::from_secs(5), async {
        // The registered gate is taken by the completion branch before it awaits a permit.
        while !shutdown_answer::popup_completion_paused_for_test() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the popup completion is selected before shutdown");
    handler.close_normal_request_admission();
    drop(shutdown_tx);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !shutdown_answer::connection_shutdown_observed_for_test() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the connection receives shutdown before the popup result resumes");
    eprintln!("popup completion selected; admission closed; connection shutdown observed; resume");
    gate.add_permits(1);
    let response = shutdown_response(&mut client).await?;
    let stdout = response
        .output
        .as_ref()
        .map(|output| String::from_utf8_lossy(output.stdout()).into_owned());
    assert_eq!(
        stdout.as_deref(),
        Some("BEFORE\n"),
        "the first file's output is kept and the second file prints nothing: {response:?}"
    );
    assert!(
        String::from_utf8_lossy(&response.stderr).contains("no-such-session-r4"),
        "the error acquired before the popup is kept: {response:?}"
    );
    assert_eq!(
        response.exit_status,
        Some(1),
        "the status acquired before the popup is kept (tmux 3.7b loses it and exits 0 at shutdown)"
    );
    finish_connection(connection).await?;
    let _ = handler
        .handle(cli_command(format!("display-popup -C -c {ATTACH_PID}")))
        .await;
    drop(control_rx);
    assert!(
        !consumer.exists(),
        "no command of the next file runs during the shutdown"
    );
    for path in [&ready, &first, &second] {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
