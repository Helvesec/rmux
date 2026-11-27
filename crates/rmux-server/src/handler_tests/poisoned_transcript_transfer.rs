//! A split or a swap that names a poisoned pane checks before it changes
//! anything: it is refused with the typed error and every layout stays as it
//! was. A split of a healthy pane and a swap of two healthy panes are served
//! beside the poisoned pane, as tmux 3.7b, join-pane and move-pane serve
//! them, and leave what the same requests leave on a server never poisoned.

use super::*;
use rmux_proto::{RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse, SplitDirection};

/// Every pane of every session with its place, size and window layout.
const LAYOUT: &[&str] = &[
    "list-panes",
    "-a",
    "-F",
    "#{session_name}:#{window_index}.#{pane_index}=#{pane_id}:#{pane_width}x#{pane_height}:#{window_zoomed_flag}:#{window_layout}",
];

/// Splits and swaps that name `poisoned:0.0`: by its place, as the active
/// pane of its session, through `linker:9`, its window linked into another
/// session, and through `grouped`, a session grouped with the poisoned one;
/// and splits that resize it: full size in its window, and zooming it as the
/// active pane of its window.
const REFUSED: &[&[&str]] = &[
    &["split-window", "-d", "-t", "poisoned:0.0"],
    &["split-window", "-t", "poisoned:0.0"],
    &["split-window", "-d", "-t", "poisoned"],
    &["split-window", "-d", "-t", "linker:9.0"],
    &["split-window", "-d", "-t", "grouped:0.0"],
    &["split-window", "-d", "-f", "-t", "poisoned:0.1"],
    &["split-window", "-d", "-f", "-h", "-t", "linker:9.1"],
    &["split-window", "-d", "-Z", "-t", "poisoned:0.1"],
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

/// Splits of a healthy pane that leave the poisoned pane at its size, after
/// a detached split in its window while it was still the active pane: in
/// its window once a split made the new pane active, as the session's active
/// pane, in another window of its session, full size in another window,
/// before the
/// pane, and through `linker:9`, `grouped`, `borrower:9` and `lender:0`,
/// linked into the poisoned session as `poisoned:5`; last, a split that
/// zooms the new pane.
const SPLITS: &[&[&str]] = &[
    &["split-window", "-t", "poisoned:0.1"],
    &["split-window", "-d", "-h", "-t", "poisoned"],
    &["split-window", "-d", "-h", "-t", "poisoned:1.0"],
    &["split-window", "-d", "-f", "-h", "-t", "poisoned:1.1"],
    &["split-window", "-d", "-b", "-h", "-t", "linker:9.1"],
    &["split-window", "-d", "-t", "grouped:1.0"],
    &["split-window", "-d", "-t", "borrower:9.0"],
    &["split-window", "-d", "-h", "-t", "lender:0.0"],
    &["resize-pane", "-Z", "-t", "poisoned:0.1"],
    &["split-window", "-d", "-t", "poisoned:0.1"],
    &["split-window", "-Z", "-h", "-t", "poisoned:0.1"],
];

fn execution(arguments: &[&str]) -> Request {
    Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
        arguments.iter().map(|s| (*s).to_owned()).collect(),
        None,
    )))
}

/// `split-window` from the protocol with a session target: it divides the
/// active pane of the session's active window.
pub(super) async fn session_split(
    handler: &RequestHandler,
    session: &str,
) -> Result<String, String> {
    match handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name(session)),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await
    {
        Response::SplitWindow(response) => Ok(response.pane.to_string()),
        Response::Error(error) => Err(error.error.to_string()),
        other => Err(format!("{other:?}")),
    }
}

/// Runs one request in its own task, as one daemon connection.
pub(super) async fn outcome(
    handler: &RequestHandler,
    arguments: &[&str],
) -> Result<String, String> {
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

pub(super) async fn layout(handler: &RequestHandler) -> String {
    outcome(handler, LAYOUT)
        .await
        .expect("list-panes -a lists a poisoned pane")
}

/// Records in `wrong` a served request that answered or left the layouts
/// otherwise than on the never poisoned `twin`, or changed nothing.
async fn served_as_unpoisoned(
    servers: (&RequestHandler, &RequestHandler),
    request: String,
    before: String,
    (answered, expected): (Result<String, String>, Result<String, String>),
    wrong: &mut Vec<String>,
) {
    let (handler, twin) = servers;
    if answered.is_err() || answered != expected {
        wrong.push(format!(
            "{request} answered {answered:?}, unpoisoned {expected:?}"
        ));
    }
    let after = layout(handler).await;
    let unpoisoned = layout(twin).await;
    if after != unpoisoned || after == before {
        wrong.push(format!(
            "{request} left\n{after}not\n{unpoisoned}from\n{before}"
        ));
    }
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
    // The poisoned pane is the active pane of its session.
    let before = layout(&handler).await;
    let answered = session_split(&handler, "poisoned").await;
    if answered.as_ref() != Err(&unavailable) || layout(&handler).await != before {
        wrong.push(format!(
            "protocol split of session poisoned answered {answered:?}"
        ));
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
    // While the poisoned pane is still the active pane of its window, a
    // detached split of its neighbour leaves it at its size.
    let request = ["split-window", "-d", "-h", "-t", "poisoned:0.1"];
    let before = layout(&handler).await;
    let answers = (
        outcome(&handler, &request).await,
        outcome(&twin, &request).await,
    );
    let servers = (&handler, &twin);
    served_as_unpoisoned(servers, format!("{request:?}"), before, answers, &mut wrong).await;
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
    for (step, request) in SPLITS.iter().enumerate() {
        let before = layout(&handler).await;
        let answers = (
            outcome(&handler, request).await,
            outcome(&twin, request).await,
        );
        served_as_unpoisoned(servers, format!("{request:?}"), before, answers, &mut wrong).await;
        if step == 0 {
            // That split made a healthy pane the session's active pane.
            let before = layout(&handler).await;
            let answers = (
                session_split(&handler, "poisoned").await,
                session_split(&twin, "poisoned").await,
            );
            let request = "protocol split of session poisoned".to_owned();
            served_as_unpoisoned(servers, request, before, answers, &mut wrong).await;
        }
    }
    assert_eq!(wrong, Vec::<String>::new());

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

#[tokio::test]
async fn poisoned_zoomed_pane_refuses_a_split_that_unzooms_it() {
    split_behind_a_zoomed_pane(false).await;
}

#[tokio::test]
async fn poisoned_zoomed_pane_and_output_refuse_a_split_that_unzooms_them() {
    split_behind_a_zoomed_pane(true).await;
}

/// `z:0.0` is zoomed over `z:0.1`. A split of the hidden pane unzooms the
/// window and resizes the zoomed one: served while it is healthy, refused
/// without any change once it is poisoned.
async fn split_behind_a_zoomed_pane(output_too: bool) {
    const SPLIT: &[&str] = &["split-window", "-d", "-t", "z:0.1"];
    let zoomed = PaneTarget::with_window(session_name("z"), 0, 0);
    let mut answers = Vec::new();
    for poisoned in [false, true] {
        let handler = RequestHandler::new();
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name("z"),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
        );
        for setup in [
            &["split-window", "-d", "-t", "z:0.0"][..],
            &["resize-pane", "-Z", "-t", "z:0.0"],
        ] {
            outcome(&handler, setup)
                .await
                .unwrap_or_else(|error| panic!("{setup:?}: {error}"));
        }
        if poisoned {
            handler.poison_pane_transcript_for_test(&zoomed).await;
            if output_too {
                handler
                    .state
                    .lock()
                    .await
                    .pane_output_for_target(zoomed.session_name(), 0, 0)
                    .expect("zoomed pane output")
                    .poison_state_for_test();
            }
        }
        let before = layout(&handler).await;
        let answered = outcome(&handler, SPLIT).await;
        answers.push((answered, before == layout(&handler).await));
    }
    assert_eq!(
        answers,
        [
            (Ok(String::new()), false),
            (Err("pane %0 unavailable".to_owned()), true)
        ]
    );
}
