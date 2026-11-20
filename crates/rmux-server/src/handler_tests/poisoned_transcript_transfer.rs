//! A split in the session of a poisoned pane, or a swap that names it, checks
//! before it changes anything. Once committed, a split resizes and refreshes
//! every pane of its session through the fail-fast lock, so a later failure
//! would answer an error for a change already made. It is refused with the
//! typed error and every layout stays as it was. A swap of two healthy panes
//! is served beside the poisoned pane, as tmux 3.7b, join-pane and move-pane
//! serve it, and the same requests in a healthy session still succeed.

use super::*;
use rmux_proto::{RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse};

/// Every pane of every session with its place, size and window layout.
const LAYOUT: &[&str] = &[
    "list-panes",
    "-a",
    "-F",
    "#{session_name}:#{window_index}.#{pane_index}=#{pane_id}:#{pane_width}x#{pane_height}:#{window_zoomed_flag}:#{window_layout}",
];

/// Splits that reach the session of `poisoned:0.0` and swaps that name it:
/// a pane in its window, one in another window, the poisoned pane itself,
/// and requests from `linker`, whose window `linker:9` is the poisoned
/// pane's window linked into another session, and from `grouped`, a session
/// grouped with the poisoned one.
const REFUSED: &[&[&str]] = &[
    &["split-window", "-d", "-t", "poisoned:0.1"],
    &["split-window", "-d", "-t", "poisoned:1.0"],
    &["split-window", "-d", "-t", "poisoned:0.0"],
    &[
        "swap-pane",
        "-d",
        "-s",
        "poisoned:0.0",
        "-t",
        "poisoned:0.1",
    ],
    &[
        "swap-pane",
        "-d",
        "-s",
        "poisoned:1.0",
        "-t",
        "poisoned:0.0",
    ],
    &["swap-pane", "-d", "-s", "poisoned:0.0", "-t", "healthy:0.0"],
    &["swap-pane", "-d", "-s", "healthy:0.0", "-t", "poisoned:0.0"],
    &["split-window", "-d", "-t", "linker:9.1"],
    &["swap-pane", "-d", "-s", "linker:9.0", "-t", "linker:9.1"],
    &["swap-pane", "-s", "linker:0.0", "-t", "linker:9.0"],
    &["swap-pane", "-d", "-s", "grouped:0.0", "-t", "grouped:1.1"],
];

/// Swaps of two healthy panes whose windows' linked families hold the
/// poisoned pane: in its window, in its session, with a healthy session from
/// either side, and from another session that shares a window with it, in
/// each direction: `linker:9` is the poisoned pane's window, `borrower:9` is
/// the other one, and `lender:0` is a window of a healthy session linked into
/// the poisoned one as `poisoned:5`, and in `grouped`, a session grouped with
/// the poisoned one. Each runs once without and once with `-d`, which swaps
/// the two panes back.
const SERVED: &[&[&str]] = &[
    &["swap-pane", "-s", "poisoned:1.0", "-t", "poisoned:1.1"],
    &["swap-pane", "-s", "poisoned:0.1", "-t", "poisoned:1.0"],
    &["swap-pane", "-s", "poisoned:1.0", "-t", "healthy:0.0"],
    &["swap-pane", "-s", "healthy:0.0", "-t", "poisoned:0.1"],
    &["swap-pane", "-s", "linker:0.0", "-t", "linker:9.1"],
    &["swap-pane", "-s", "borrower:9.0", "-t", "borrower:9.1"],
    &["swap-pane", "-s", "borrower:0.0", "-t", "borrower:9.0"],
    &["swap-pane", "-s", "borrower:9.0", "-t", "healthy:0.0"],
    &["swap-pane", "-s", "healthy:0.0", "-t", "borrower:9.1"],
    &["swap-pane", "-s", "lender:0.0", "-t", "lender:0.1"],
    &["swap-pane", "-s", "grouped:1.0", "-t", "grouped:0.1"],
];

fn execution(arguments: &[&str]) -> Request {
    Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
        arguments.iter().map(|s| (*s).to_owned()).collect(),
        None,
    )))
}

/// Runs one request in its own task, as one daemon connection.
async fn outcome(handler: &RequestHandler, arguments: &[&str]) -> Result<String, String> {
    let worker = handler.clone();
    let request = execution(arguments);
    let task = tokio::spawn(async move { worker.handle(request).await });
    match tokio::time::timeout(Duration::from_secs(10), task).await {
        Err(_) => Err("no response within 10 s".to_owned()),
        Ok(Err(error)) => Err(format!("connection closed: {error}")),
        Ok(Ok(Response::RuntimeCommandExecution(result))) => match *result {
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                stdout,
                ..
            } => Ok(String::from_utf8(stdout).expect("utf-8 stdout")),
            RuntimeCommandExecutionResponse::Complete {
                error: Some(error), ..
            } => Err(error.to_string()),
            other => Err(format!("{other:?}")),
        },
        Ok(Ok(other)) => Err(format!("{other:?}")),
    }
}

async fn layout(handler: &RequestHandler) -> String {
    outcome(handler, LAYOUT)
        .await
        .expect("list-panes -a lists a poisoned pane")
}

#[tokio::test]
async fn poisoned_transcript_serves_healthy_swaps_and_refuses_what_names_it() {
    serve_healthy_swaps_around_one_poisoned_pane(false).await;
}

/// A reader that panics on its next publication poisons the output state too.
#[tokio::test]
async fn poisoned_transcript_and_output_serve_healthy_swaps_and_refuse_what_names_them() {
    serve_healthy_swaps_around_one_poisoned_pane(true).await;
}

/// Six sessions; `poisoned:0` is linked as `linker:9`, `poisoned:1` as
/// `borrower:9`, and `lender:0` as `poisoned:5`; `grouped` joins the
/// poisoned session's group.
async fn sessions_around_one_pane() -> RequestHandler {
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
    for name in ["poisoned", "healthy", "linker", "borrower", "lender"] {
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
        &["split-window", "-d", "-t", "poisoned:1.0"],
        &["split-window", "-d", "-t", "healthy:0.0"],
        &["link-window", "-d", "-s", "poisoned:0", "-t", "linker:9"],
        &["link-window", "-d", "-s", "poisoned:1", "-t", "borrower:9"],
        &["split-window", "-d", "-t", "lender:0.0"],
        &["link-window", "-d", "-s", "lender:0", "-t", "poisoned:5"],
    ] {
        outcome(&handler, setup)
            .await
            .unwrap_or_else(|error| panic!("{setup:?}: {error}"));
    }
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
                session_name: Some(session_name("grouped")),
                working_directory: None,
                detached: true,
                size: None,
                environment: None,
                group_target: Some(session_name("poisoned")),
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            })))
            .await,
    );
    handler
}

async fn serve_healthy_swaps_around_one_poisoned_pane(output_too: bool) {
    let handler = sessions_around_one_pane().await;
    // The same sessions, never poisoned: what each healthy swap must leave.
    let twin = sessions_around_one_pane().await;
    assert_eq!(layout(&handler).await, layout(&twin).await);
    let poisoned = PaneTarget::with_window(session_name("poisoned"), 0, 0);
    let pane_id = outcome(
        &handler,
        &["display-message", "-p", "-t", "poisoned:0.0", "#{pane_id}"],
    )
    .await
    .expect("pane id before poison");
    let unavailable = format!("pane {} unavailable", pane_id.trim_end());
    handler.poison_pane_transcript_for_test(&poisoned).await;
    if output_too {
        handler
            .state
            .lock()
            .await
            .pane_output_for_target(poisoned.session_name(), 0, 0)
            .expect("poisoned output")
            .poison_state_for_test();
    }

    let mut wrong = Vec::new();
    for swap in SERVED {
        let detached = [&swap[..1], &["-d"][..], &swap[1..]].concat();
        for request in [*swap, &detached[..]] {
            let before = layout(&handler).await;
            let answered = outcome(&handler, request).await;
            let expected = outcome(&twin, request).await;
            let after = layout(&handler).await;
            if answered != Ok(String::new()) || answered != expected {
                wrong.push(format!(
                    "{request:?} answered {answered:?}, unpoisoned {expected:?}"
                ));
            }
            let unpoisoned = layout(&twin).await;
            if after != unpoisoned || after == before {
                wrong.push(format!(
                    "{request:?} left\n{after}not\n{unpoisoned}from\n{before}"
                ));
            }
        }
    }
    for request in REFUSED {
        let before = layout(&handler).await;
        let answered = outcome(&handler, request).await;
        let after = layout(&handler).await;
        if answered.as_ref() != Err(&unavailable) {
            wrong.push(format!("{request:?} answered {answered:?}"));
        }
        if after != before {
            wrong.push(format!("{request:?} changed\n{before}into\n{after}"));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());

    // A split in `borrower:9` resizes `borrower` and the shared window only,
    // and refreshes neither the poisoned pane's window nor its transcripts:
    // it is served, on both aliases of the window.
    assert_eq!(
        outcome(&handler, &["split-window", "-d", "-t", "borrower:9.0"]).await,
        Ok(String::new())
    );
    let listed = layout(&handler).await;
    for alias in ["borrower:9.", "poisoned:1."] {
        let panes = listed
            .lines()
            .filter(|line| line.starts_with(alias))
            .count();
        assert_eq!(panes, 3, "{alias} after the split\n{listed}");
    }

    // The healthy session is still served and its split and swap say so.
    let healthy_panes = |listed: &str| {
        listed
            .lines()
            .filter(|line| line.starts_with("healthy:"))
            .map(|line| line.split(':').nth(1).expect("place=id").to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        outcome(&handler, &["split-window", "-d", "-t", "healthy:0.1"]).await,
        Ok(String::new())
    );
    let split = healthy_panes(&layout(&handler).await);
    assert_eq!(split.len(), 3, "{split:?}");
    assert_eq!(
        outcome(
            &handler,
            &["swap-pane", "-d", "-s", "healthy:0.0", "-t", "healthy:0.2"]
        )
        .await,
        Ok(String::new())
    );
    let swapped = healthy_panes(&layout(&handler).await);
    let place_and_id = |entry: &String| {
        entry
            .split_once('=')
            .map(|(place, id)| (place.to_owned(), id.to_owned()))
    };
    let ids = |panes: &[String], place: &str| {
        panes
            .iter()
            .filter_map(place_and_id)
            .find(|(at, _)| at == place)
            .map(|(_, id)| id)
    };
    assert_eq!(
        ids(&swapped, "0.0"),
        ids(&split, "0.2"),
        "{split:?} -> {swapped:?}"
    );
    assert_eq!(
        ids(&swapped, "0.2"),
        ids(&split, "0.0"),
        "{split:?} -> {swapped:?}"
    );
}
