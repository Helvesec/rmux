//! A window selection that names no poisoned pane is served whole.
//!
//! select-window, next-window, previous-window and last-window change the
//! session's current window, then refresh every session linked to the window
//! left and the window entered. That refresh looks for clock-mode panes in
//! the current window of each of those sessions. Through the fail-fast
//! transcript lock it made the client read "pane %N unavailable" for a
//! selection already made, whenever one of those sessions showed a poisoned
//! pane. Each link direction reaches the poisoned session: its own windows,
//! `borrower:9` (another window of the poisoned session linked elsewhere),
//! `lender:0` (a healthy window linked into the poisoned session as
//! `poisoned:5`) and `linker:9` (the poisoned pane's window linked elsewhere).
//!
//! A client attached where the poisoned pane is shown has its focus moved and
//! its view rendered by the same refresh. Both read the pane they never named,
//! and the render subscribes to the output of the pane it shows: a selection
//! seen by such a client is served whole too, as without a client.

use super::*;
use rmux_proto::{RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse};

/// Runs one command line in its own task, as one daemon connection: its
/// stdout on success, otherwise the message its client reads.
async fn answer(handler: &RequestHandler, arguments: &[&str]) -> Result<String, String> {
    let worker = handler.clone();
    let request = Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        None,
    )));
    let task = tokio::spawn(async move { worker.handle(request).await });
    match tokio::time::timeout(Duration::from_secs(10), task).await {
        Err(_) => Err("no response within 10 s".to_owned()),
        Ok(Err(error)) if error.is_panic() => Err("panicked: connection closed".to_owned()),
        Ok(Err(error)) => Err(format!("task failed: {error}")),
        Ok(Ok(Response::Error(ErrorResponse {
            error: RmuxError::Message(message),
        }))) => Err(message),
        Ok(Ok(Response::RuntimeCommandExecution(result))) => match *result {
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                stdout,
                ..
            } => Ok(String::from_utf8(stdout).expect("utf-8 stdout")),
            RuntimeCommandExecutionResponse::Complete {
                error: Some(RmuxError::Message(message)),
                ..
            } => Err(message),
            other => Err(format!("{other:?}")),
        },
        Ok(Ok(other)) => Err(format!("{other:?}")),
    }
}

/// The typed request the CLI sends for a selection.
fn typed_selection(arguments: &[&str]) -> Request {
    let window = |target: &str| {
        let (session, index) = target.split_once(':').expect("session:window");
        rmux_proto::WindowTarget::with_window(
            session_name(session),
            index.parse().expect("window index"),
        )
    };
    match arguments {
        ["select-window", "-t", target] => Request::SelectWindow(rmux_proto::SelectWindowRequest {
            target: window(target),
        }),
        ["next-window", "-t", session] => Request::NextWindow(rmux_proto::NextWindowRequest {
            target: session_name(session),
            alerts_only: false,
        }),
        ["previous-window", "-t", session] => {
            Request::PreviousWindow(rmux_proto::PreviousWindowRequest {
                target: session_name(session),
                alerts_only: false,
            })
        }
        ["last-window", "-t", session] => Request::LastWindow(rmux_proto::LastWindowRequest {
            target: session_name(session),
        }),
        other => panic!("not a selection: {other:?}"),
    }
}

/// Sends a selection as the CLI does, as its typed request, in its own task.
async fn typed_answer(handler: &RequestHandler, arguments: &[&str]) -> Result<String, String> {
    let worker = handler.clone();
    let request = typed_selection(arguments);
    let task = tokio::spawn(async move { worker.handle(request).await });
    match tokio::time::timeout(Duration::from_secs(10), task).await {
        Err(_) => Err("no response within 10 s".to_owned()),
        Ok(Err(error)) if error.is_panic() => Err("panicked: connection closed".to_owned()),
        Ok(Err(error)) => Err(format!("task failed: {error}")),
        Ok(Ok(Response::Error(ErrorResponse {
            error: RmuxError::Message(message),
        }))) => Err(message),
        Ok(Ok(
            Response::SelectWindow(_)
            | Response::NextWindow(_)
            | Response::PreviousWindow(_)
            | Response::LastWindow(_),
        )) => Ok(String::new()),
        Ok(Ok(other)) => Err(format!("{other:?}")),
    }
}

/// Every session's current and last windows, as a selection changes them,
/// then the session of each attached client, which survives the selection.
async fn selections(handler: &RequestHandler) -> String {
    let windows = answer(
        handler,
        &[
            "list-windows",
            "-a",
            "-F",
            "#{session_name}:#{window_index}#{?window_active,*,}#{?window_last_flag,-,}",
        ],
    )
    .await
    .expect("list-windows -a stays served");
    let clients = answer(
        handler,
        &["list-clients", "-F", "client on #{client_session}"],
    )
    .await
    .expect("list-clients stays served");
    windows + &clients
}

async fn run(handler: &RequestHandler, arguments: &[&str]) {
    assert_eq!(
        answer(handler, arguments).await,
        Ok(String::new()),
        "{arguments:?}"
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    /// The typed request the CLI sends.
    Typed,
    /// A queued command line, as a script or key binding runs it.
    Queued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Poison {
    /// A panic under the transcript lock, as a recovered pane reader leaves it.
    Transcript,
    /// A reader that panics on its next publication poisons the output state
    /// as well.
    TranscriptAndOutput,
}

async fn send(handler: &RequestHandler, sent: Sent, arguments: &[&str]) -> Result<String, String> {
    match sent {
        Sent::Typed => typed_answer(handler, arguments).await,
        Sent::Queued => answer(handler, arguments).await,
    }
}

/// A selection, after the selections that set its session up.
struct Case {
    before: &'static [&'static [&'static str]],
    selection: &'static [&'static str],
}

const fn case(selection: &'static [&'static str]) -> Case {
    Case {
        before: &[],
        selection,
    }
}

/// Selections from each link direction, each on a fresh server. In each
/// session the current window is 0 and the last window is the other one.
const CASES: &[Case] = &[
    // The poisoned session itself, leaving and entering the poisoned window.
    case(&["select-window", "-t", "poisoned:1"]),
    case(&["next-window", "-t", "poisoned"]),
    case(&["previous-window", "-t", "poisoned"]),
    case(&["last-window", "-t", "poisoned"]),
    Case {
        before: &[&["select-window", "-t", "poisoned:1"]],
        selection: &["select-window", "-t", "poisoned:0"],
    },
    Case {
        before: &[&["select-window", "-t", "poisoned:1"]],
        selection: &["last-window", "-t", "poisoned"],
    },
    // A session that borrows another window of the poisoned session.
    case(&["select-window", "-t", "borrower:9"]),
    case(&["next-window", "-t", "borrower"]),
    case(&["previous-window", "-t", "borrower"]),
    case(&["last-window", "-t", "borrower"]),
    Case {
        before: &[&["select-window", "-t", "borrower:9"]],
        selection: &["select-window", "-t", "borrower:0"],
    },
    // A session whose window the poisoned session borrows.
    case(&["select-window", "-t", "lender:1"]),
    case(&["next-window", "-t", "lender"]),
    case(&["previous-window", "-t", "lender"]),
    case(&["last-window", "-t", "lender"]),
    // A session that links the poisoned pane's own window.
    case(&["select-window", "-t", "linker:9"]),
    case(&["next-window", "-t", "linker"]),
    case(&["last-window", "-t", "linker"]),
    Case {
        before: &[&["select-window", "-t", "linker:9"]],
        selection: &["select-window", "-t", "linker:0"],
    },
    // A session that links nothing of the poisoned one.
    case(&["select-window", "-t", "healthy:1"]),
    case(&["next-window", "-t", "healthy"]),
    case(&["last-window", "-t", "healthy"]),
];

/// Builds the sessions and each session's current and last windows.
async fn linked_sessions(handler: &RequestHandler) {
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
    for name in ["poisoned", "borrower", "lender", "linker", "healthy"] {
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
    for setup in [
        &["split-window", "-d", "-t", "poisoned:0.0"][..],
        &["new-window", "-d", "-t", "poisoned:1"],
        &["link-window", "-d", "-s", "poisoned:1", "-t", "borrower:9"],
        &["new-window", "-d", "-t", "borrower:12"],
        &["new-window", "-d", "-t", "lender:1"],
        &["link-window", "-d", "-s", "lender:0", "-t", "poisoned:5"],
        &["link-window", "-d", "-s", "poisoned:0", "-t", "linker:9"],
        &["new-window", "-d", "-t", "healthy:1"],
        &["select-window", "-t", "poisoned:1"],
        &["select-window", "-t", "poisoned:0"],
        &["select-window", "-t", "borrower:9"],
        &["select-window", "-t", "borrower:0"],
        &["select-window", "-t", "lender:1"],
        &["select-window", "-t", "lender:0"],
        &["select-window", "-t", "linker:9"],
        &["select-window", "-t", "linker:0"],
        &["select-window", "-t", "healthy:1"],
        &["select-window", "-t", "healthy:0"],
    ] {
        run(handler, setup).await;
    }
}

async fn poison(handler: &RequestHandler, poison: Poison) {
    let poisoned = PaneTarget::with_window(session_name("poisoned"), 0, 0);
    handler.poison_pane_transcript_for_test(&poisoned).await;
    if poison == Poison::TranscriptAndOutput {
        handler
            .state
            .lock()
            .await
            .pane_output_for_target(poisoned.session_name(), 0, 0)
            .expect("poisoned output")
            .poison_state_for_test();
    }
}

/// Runs every case on a healthy server, then on a server where
/// `poisoned:0.0` is poisoned after its case's setup: each selection must be
/// served and leave every session's windows as the healthy server does. A
/// client is attached to each session of `attached` on both servers.
async fn serve_selections_around_one_poisoned_pane(sent: Sent, kind: Poison, attached: &[&str]) {
    let mut differences = Vec::new();
    for Case { before, selection } in CASES {
        let mut outcomes = Vec::new();
        for poisoned in [false, true] {
            let handler = RequestHandler::new();
            linked_sessions(&handler).await;
            let (control_tx, _control_rx) = mpsc::unbounded_channel();
            for (offset, name) in (0_u32..).zip(attached) {
                let pid = std::process::id().saturating_add(2285 + offset);
                handler
                    .register_attach(pid, session_name(name), control_tx.clone())
                    .await;
            }
            // Selections settle the new clients' focus as an attach does,
            // and leave the current and last windows as they were.
            for name in attached {
                run(&handler, &["last-window", "-t", name]).await;
                run(&handler, &["last-window", "-t", name]).await;
            }
            for step in *before {
                run(&handler, step).await;
            }
            if poisoned {
                poison(&handler, kind).await;
            }
            let answered = send(&handler, sent, selection).await;
            outcomes.push((answered, selections(&handler).await));
        }
        let (healthy, poisoned) = (&outcomes[0], &outcomes[1]);
        assert_eq!(
            healthy.0,
            Ok(String::new()),
            "{selection:?} on a healthy server"
        );
        if poisoned != healthy {
            differences.push(format!(
                "{selection:?} after {before:?}: {:?}\n{}\nhealthy server:\n{}",
                poisoned.0, poisoned.1, healthy.1
            ));
        }
    }
    assert_eq!(differences, Vec::<String>::new(), "{sent:?} {kind:?}");
}

#[tokio::test]
async fn typed_window_selection_around_a_poisoned_transcript_is_served() {
    serve_selections_around_one_poisoned_pane(Sent::Typed, Poison::Transcript, &[]).await;
}

#[tokio::test]
async fn typed_window_selection_around_a_poisoned_transcript_and_output_is_served() {
    serve_selections_around_one_poisoned_pane(Sent::Typed, Poison::TranscriptAndOutput, &[]).await;
}

#[tokio::test]
async fn queued_window_selection_around_a_poisoned_transcript_is_served() {
    serve_selections_around_one_poisoned_pane(Sent::Queued, Poison::Transcript, &[]).await;
}

/// Clients attached to sessions that share a window with the poisoned one,
/// and never show the poisoned pane, are refreshed by the selection, and the
/// selection is still served.
#[tokio::test]
async fn window_selection_refreshing_clients_around_a_poisoned_pane_is_served() {
    serve_selections_around_one_poisoned_pane(
        Sent::Typed,
        Poison::TranscriptAndOutput,
        &["borrower", "lender"],
    )
    .await;
}

/// A client attached to the poisoned session shows the poisoned pane, and the
/// client of `linker` shows it once `linker:9` is selected. Their focus and
/// their views are settled around the selection, which is still served whole
/// and leaves every window as on a healthy server with the same clients.
#[tokio::test]
async fn typed_window_selection_seen_by_clients_of_the_poisoned_pane_is_served() {
    serve_selections_around_one_poisoned_pane(
        Sent::Typed,
        Poison::TranscriptAndOutput,
        &["poisoned", "linker"],
    )
    .await;
}

/// The same clients, for a selection run as a queued command line.
#[tokio::test]
async fn queued_window_selection_seen_by_clients_of_the_poisoned_pane_is_served() {
    serve_selections_around_one_poisoned_pane(
        Sent::Queued,
        Poison::Transcript,
        &["poisoned", "linker"],
    )
    .await;
}
