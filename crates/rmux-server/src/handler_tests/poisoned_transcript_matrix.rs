//! A poisoned pane must not fail requests that never name it.
//!
//! The same request matrix runs on a healthy server, where every entry must
//! succeed, and again after one pane's transcript and output state are
//! poisoned, as a panic under its transcript lock leaves them. Formats expand
//! every registered variable, so a future format, listing or option sweep that
//! reads a pane through the fail-fast lock fails here without any injection.

use super::*;
use rmux_proto::{RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse};

/// Requests that name no pane of the poisoned session. Transfers and option
/// sweeps run in an order that leaves `healthy` as it found it.
const MATRIX: &[&[&str]] = &[
    &["list-sessions"],
    &["list-windows", "-a"],
    &["list-panes", "-a"],
    &["list-panes", "-s", "-t", "healthy"],
    &["list-clients"],
    &["list-buffers"],
    &["list-keys"],
    &["list-commands"],
    &["show-options", "-g"],
    &["show-options", "-s"],
    &["show-options", "-g", "-w"],
    &["show-hooks", "-g"],
    &["show-environment", "-g"],
    &["show-messages"],
    &["has-session", "-t", "healthy"],
    &["display-message", "-a", "-t", "healthy"],
    &["choose-tree", "-t", "healthy"],
    &["send-keys", "-t", "healthy", "q"],
    &["choose-tree", "-s", "-t", "healthy"],
    &["send-keys", "-t", "healthy", "q"],
    &["set-option", "-g", "allow-rename", "off"],
    &["set-option", "-g", "-w", "allow-rename", "on"],
    &["set-option", "-g", "scroll-on-clear", "off"],
    &["set-option", "-g", "cursor-style", "block"],
    &["set-option", "-g", "history-limit", "3000"],
    &["set-option", "-g", "alternate-screen", "off"],
    &["set-option", "-g", "allow-set-title", "off"],
    &["set-option", "-s", "extended-keys", "on"],
    &["set-option", "-g", "status-left", "x"],
    &["split-window", "-d", "-t", "healthy:0"],
    &["swap-pane", "-d", "-s", "healthy:0.0", "-t", "healthy:0.1"],
    &["break-pane", "-d", "-s", "healthy:0.1", "-t", "healthy:5"],
    &["join-pane", "-d", "-s", "healthy:5.0", "-t", "healthy:0"],
    &["select-layout", "-t", "healthy:0", "tiled"],
    &["kill-pane", "-t", "healthy:0.1"],
    &["new-window", "-d", "-t", "healthy:6"],
    &["rename-window", "-t", "healthy:6", "renamed"],
    &["move-window", "-s", "healthy:6", "-t", "healthy:7"],
    &["kill-window", "-t", "healthy:7"],
    &["resize-window", "-t", "healthy", "-x", "90", "-y", "30"],
    &["run-shell", "true"],
    &["if-shell", "true", "list-sessions"],
    &["wait-for", "-S", "poisoned-matrix"],
    &["set-buffer", "abc"],
    &["server-access", "-l"],
    &["refresh-client", "-S"],
];

fn owned(arguments: &[&str]) -> Vec<String> {
    arguments.iter().map(|s| (*s).to_owned()).collect()
}

fn every_format_variable() -> String {
    rmux_core::formats::format_variable_descriptors()
        .map(|descriptor| format!("{}=#{{{}}}", descriptor.name(), descriptor.name()))
        .collect::<Vec<_>>()
        .join("|")
}

/// The matrix for the given attached clients. `refresh-client -S` names the
/// requesting client, which a second attached client makes ambiguous on the
/// healthy server already, so it only runs with one client.
fn matrix(attached: &[&str]) -> Vec<Vec<String>> {
    let every = every_format_variable();
    let mut cases: Vec<Vec<String>> = MATRIX
        .iter()
        .filter(|arguments| attached.len() == 1 || arguments[0] != "refresh-client")
        .map(|arguments| owned(arguments))
        .collect();
    for listing in [
        &["list-sessions", "-F"][..],
        &["list-windows", "-a", "-F"],
        &["list-panes", "-a", "-F"],
        &["list-clients", "-F"],
        &["choose-tree", "-t", "healthy", "-F"],
    ] {
        let mut case = owned(listing);
        case.push(every.clone());
        cases.push(case);
    }
    cases.push(owned(&["send-keys", "-t", "healthy", "q"]));
    let mut loops = owned(&["display-message", "-t", "healthy", "-p"]);
    loops.push(format!("#{{S:#{{W:#{{P:{every}}}}}}}"));
    cases.push(loops);
    cases
}

fn execution(arguments: Vec<String>) -> Request {
    Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
        arguments, None,
    )))
}

/// Runs one request in its own task, as one daemon connection, and reports
/// anything but a completed successful command.
async fn failure(handler: &RequestHandler, arguments: &[String]) -> Option<String> {
    let worker = handler.clone();
    let request = execution(arguments.to_vec());
    let task = tokio::spawn(async move { worker.handle(request).await });
    let outcome = match tokio::time::timeout(Duration::from_secs(10), task).await {
        Err(_) => "no response within 10 s".to_owned(),
        Ok(Err(error)) if error.is_panic() => "panicked: connection closed".to_owned(),
        Ok(Err(error)) => format!("task failed: {error}"),
        Ok(Ok(Response::RuntimeCommandExecution(result))) => match *result {
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                ..
            } => return None,
            other => format!("{other:?}"),
        },
        Ok(Ok(other)) => format!("{other:?}"),
    };
    let command = arguments.iter().take(4).cloned().collect::<Vec<_>>();
    Some(format!("{command:?}: {outcome}"))
}

async fn failures(handler: &RequestHandler, cases: &[Vec<String>]) -> Vec<String> {
    let mut failed = Vec::new();
    for case in cases {
        failed.extend(failure(handler, case).await);
    }
    // The CLI sends new-session as a typed request, not as an execution.
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("fresh"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    if !matches!(created, Response::NewSession(_)) {
        failed.push(format!("new-session: {created:?}"));
    }
    failed.extend(failure(handler, &owned(&["kill-session", "-t", "fresh"])).await);
    failed
}

async fn command_stdout(handler: &RequestHandler, arguments: &[&str]) -> String {
    let Response::RuntimeCommandExecution(result) =
        handler.handle(execution(owned(arguments))).await
    else {
        panic!("{arguments:?} answers an execution");
    };
    let RuntimeCommandExecutionResponse::Complete { stdout, .. } = *result else {
        panic!("{arguments:?} completes");
    };
    String::from_utf8(stdout).expect("utf-8 stdout")
}

#[tokio::test]
async fn poisoned_pane_leaves_every_request_that_does_not_name_it_served() {
    serve_the_matrix_around_one_poisoned_pane(PoisonedState::TranscriptAndOutput, &["healthy"])
        .await;
}

/// Once the pane reader recovers its publication, a poisoned transcript no
/// longer poisons the output state: the matrix holds for that state too.
#[tokio::test]
async fn poisoned_transcript_alone_leaves_every_request_that_does_not_name_it_served() {
    serve_the_matrix_around_one_poisoned_pane(PoisonedState::TranscriptOnly, &["healthy"]).await;
}

/// A client attached to the poisoned session shows the poisoned pane. Every
/// request refreshes the clients' views and settles their focus; neither may
/// read that pane through the fail-fast lock for a request that never named
/// it, and the client stays attached.
#[tokio::test]
async fn poisoned_pane_shown_to_a_client_leaves_every_request_that_does_not_name_it_served() {
    serve_the_matrix_around_one_poisoned_pane(
        PoisonedState::TranscriptAndOutput,
        &["healthy", "poisoned"],
    )
    .await;
}

#[tokio::test]
async fn poisoned_transcript_shown_to_a_client_leaves_every_request_that_does_not_name_it_served() {
    serve_the_matrix_around_one_poisoned_pane(
        PoisonedState::TranscriptOnly,
        &["healthy", "poisoned"],
    )
    .await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PoisonedState {
    TranscriptOnly,
    TranscriptAndOutput,
}

async fn serve_the_matrix_around_one_poisoned_pane(
    poisoned_state: PoisonedState,
    attached: &[&str],
) {
    let handler = RequestHandler::new();
    // A quiet child keeps the live pane readers out of this test's timing.
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultCommand,
                value: crate::test_shell::stdin_discard_command(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    for name in ["poisoned", "healthy"] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name(name),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
        );
    }
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    for (offset, name) in (0_u32..).zip(attached) {
        handler
            .register_attach(
                std::process::id().saturating_add(2229 + offset),
                session_name(name),
                control_tx.clone(),
            )
            .await;
    }
    let cases = matrix(attached);
    assert_eq!(failures(&handler, &cases).await, Vec::<String>::new());

    let poisoned = PaneTarget::with_window(session_name("poisoned"), 0, 0);
    let (transcript, output) = {
        let state = handler.state.lock().await;
        (
            state
                .transcript_handle(&poisoned)
                .expect("poisoned transcript"),
            state
                .pane_output_for_target(poisoned.session_name(), 0, 0)
                .expect("poisoned output"),
        )
    };
    // A panic under the transcript lock poisons it; a reader that panics on
    // its next publication poisons the output state as well.
    transcript.poison_for_test();
    if poisoned_state == PoisonedState::TranscriptAndOutput {
        output.poison_state_for_test();
    }

    assert_eq!(failures(&handler, &cases).await, Vec::<String>::new());
    let listed = command_stdout(
        &handler,
        &["list-panes", "-a", "-F", "#{pane_id}:#{history_limit}"],
    )
    .await;
    let (unavailable, available): (Vec<_>, Vec<_>) =
        listed.lines().partition(|line| line.starts_with("%0:"));
    assert_eq!(
        unavailable,
        ["%0:"],
        "the poisoned pane is listed with its transcript values unavailable"
    );
    assert!(
        !available.is_empty() && available.iter().all(|line| line.ends_with(":3000")),
        "healthy panes keep their values: {listed:?}"
    );
    assert_eq!(
        command_stdout(&handler, &["list-clients", "-F", "#{session_name}"]).await,
        attached
            .iter()
            .map(|name| format!("{name}\n"))
            .collect::<String>(),
        "the attached clients survive the matrix"
    );
    assert!(
        std::panic::catch_unwind(|| drop(transcript.lock())).is_err(),
        "the poisoned pane itself keeps failing fast"
    );
}
