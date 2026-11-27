//! A pane command that names a healthy pane beside a poisoned one is served.
//!
//! In tmux 3.7b, `select-pane -t p:0.1`, `last-pane -t p:0` and
//! `kill-pane -t p:0.1` succeed in a window whatever its other panes hold.
//! Each one resizes the window's panes once its change is made, and that
//! resize read the alternate-screen and copy-mode flags of every pane through
//! the fail-fast transcript lock. With `%0` poisoned, the client read
//! "pane %0 unavailable" for a selection or a kill already made, with or
//! without a client attached. Other layout changes that resize the window
//! failed the same way.
//!
//! The resize now reads a poisoned pane as neither alternate nor copying, as
//! formats observe it. A command that names the poisoned pane and resizes its
//! window is refused before its change stays committed, and leaves every pane
//! as it found it; one that resizes nothing answers as before.
//!
//! `join-pane` and `move-pane` name two panes and resize both windows, yet one
//! naming `%0`, as its source or its target, moved and resized `%0` and
//! answered success. Such a join or move is now refused before anything
//! moves, as `break-pane` and `swap-pane` naming `%0` are. A healthy pane
//! still joins or leaves the poisoned pane's window, from any session.
//!
//! The four transfers share one contract for the panes they name. tmux 3.7b
//! answers one that moves nothing whatever its panes hold: `%0` swapped with
//! itself answers success, and a break onto a taken window index answers its
//! error, each without a change. `join-pane` and `move-pane` of `%0` onto
//! itself already answered their own error; `swap-pane` and `break-pane`
//! answered "pane %0 unavailable" instead. They now answer as on a healthy
//! server, and a swap that unzooms the window of `%0` stays refused.

use super::*;
use rmux_proto::{RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    /// The typed request the CLI sends.
    Typed,
    /// The SDK's selection of a pane by reference.
    Sdk,
    /// A queued command line, as a script or key binding runs it.
    Queued,
}

fn pane(target: &str) -> PaneTarget {
    let (session, rest) = target.split_once(':').expect("session:window.pane");
    let (window, pane) = rest.split_once('.').expect("window.pane");
    PaneTarget::with_window(
        session_name(session),
        window.parse().expect("window index"),
        pane.parse().expect("pane index"),
    )
}

fn window(target: &str) -> rmux_proto::WindowTarget {
    let (session, index) = target.split_once(':').expect("session:window");
    rmux_proto::WindowTarget::with_window(session_name(session), index.parse().expect("index"))
}

/// The typed request the CLI sends for the three pane commands of tmux's own
/// reproduction and for the pane transfers, and the SDK's for a selection; the
/// other cases run as queued command lines.
fn typed(sent: Sent, arguments: &[&str]) -> Option<Request> {
    if sent == Sent::Sdk {
        let ["select-pane", "-t", target] = arguments else {
            return None;
        };
        return Some(Request::PaneSelect(rmux_proto::PaneSelectRequest {
            target: rmux_proto::PaneTargetRef::Slot(pane(target)),
            title: None,
        }));
    }
    Some(match arguments {
        ["select-pane", "-t", target] => {
            Request::SelectPane(Box::new(rmux_proto::SelectPaneRequest {
                target: pane(target),
                title: None,
                input_disabled: None,
                preserve_zoom: false,
                style: None,
            }))
        }
        ["last-pane", "-t", target] => Request::LastPane(rmux_proto::LastPaneRequest {
            target: window(target),
            preserve_zoom: false,
            input_disabled: None,
        }),
        ["kill-pane", "-t", target] => Request::KillPane(rmux_proto::KillPaneRequest {
            target: pane(target),
            kill_all_except: false,
        }),
        ["join-pane", "-d", "-s", source, "-t", target] => {
            Request::JoinPane(rmux_proto::JoinPaneRequest {
                source: pane(source),
                target: pane(target),
                direction: rmux_proto::SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            })
        }
        ["move-pane", "-d", "-s", source, "-t", target] => {
            Request::MovePane(rmux_proto::MovePaneRequest {
                source: pane(source),
                target: pane(target),
                direction: rmux_proto::SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            })
        }
        ["break-pane", "-d", "-s", source] => {
            Request::BreakPane(Box::new(rmux_proto::BreakPaneRequest {
                source: pane(source),
                target: None,
                name: None,
                detached: true,
                after: false,
                before: false,
                print_target: false,
                format: None,
            }))
        }
        ["break-pane", "-d", "-s", source, "-t", target] => {
            Request::BreakPane(Box::new(rmux_proto::BreakPaneRequest {
                source: pane(source),
                target: Some(window(target)),
                name: None,
                detached: true,
                after: false,
                before: false,
                print_target: false,
                format: None,
            }))
        }
        ["swap-pane", "-d", "-s", source, "-t", target] => {
            Request::SwapPane(rmux_proto::SwapPaneRequest {
                source: pane(source),
                target: pane(target),
                direction: None,
                detached: true,
                preserve_zoom: false,
            })
        }
        _ => return None,
    })
}

/// Runs one request in its own task, as one daemon connection: its stdout on
/// success, otherwise the message its client reads.
async fn answer(
    handler: &RequestHandler,
    sent: Sent,
    arguments: &[&str],
) -> Result<String, String> {
    let worker = handler.clone();
    let request = match sent {
        Sent::Typed | Sent::Sdk => typed(sent, arguments).expect("a typed pane command"),
        Sent::Queued => {
            Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
                arguments
                    .iter()
                    .map(|argument| (*argument).to_owned())
                    .collect(),
                None,
            )))
        }
    };
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
        Ok(Ok(
            Response::SelectPane(_)
            | Response::LastPane(_)
            | Response::KillPane(_)
            | Response::JoinPane(_)
            | Response::MovePane(_)
            | Response::BreakPane(_)
            | Response::SwapPane(_),
        )) => Ok(String::new()),
        Ok(Ok(other)) => Err(format!("{other:?}")),
    }
}

async fn run(handler: &RequestHandler, arguments: &[&str]) {
    assert_eq!(
        answer(handler, Sent::Queued, arguments).await,
        Ok(String::new()),
        "{arguments:?}"
    );
}

/// Every pane with its identity, active and last flags and size, then the
/// session of each attached client, which survives the command.
async fn panes(handler: &RequestHandler) -> String {
    let panes = answer(
        handler,
        Sent::Queued,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}:#{window_index}.#{pane_index} #{pane_id}\
             #{?pane_active,*,}#{?pane_last,-,} #{pane_width}x#{pane_height}",
        ],
    )
    .await
    .expect("list-panes -a stays served");
    let clients = answer(
        handler,
        Sent::Queued,
        &["list-clients", "-F", "client on #{client_session}"],
    )
    .await
    .expect("list-clients stays served");
    panes + &clients
}

/// `p:0.0` (`%0`, later poisoned) is active beside `p:0.1`; `p:1` and the
/// session `other` hold one pane each.
/// The receiver of the attached client, which stays attached while it lives.
async fn window_beside_a_poisoned_pane(
    handler: &RequestHandler,
    attached: bool,
) -> Option<mpsc::UnboundedReceiver<AttachControl>> {
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
    for name in ["p", "other"] {
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
    run(handler, &["split-window", "-d", "-t", "p:0.0"]).await;
    run(handler, &["new-window", "-d", "-t", "p:1"]).await;
    if !attached {
        return None;
    }
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(
            std::process::id().saturating_add(2296),
            session_name("p"),
            control_tx,
        )
        .await;
    // Settle the new client's focus as an attach does, keeping p:0 current.
    run(handler, &["select-window", "-t", "p:1"]).await;
    run(handler, &["select-window", "-t", "p:0"]).await;
    Some(control_rx)
}

/// A command after the commands that set its window up.
struct Case {
    before: &'static [&'static [&'static str]],
    command: &'static [&'static str],
}

const fn case(command: &'static [&'static str]) -> Case {
    Case {
        before: &[],
        command,
    }
}

const SELECTED_P01: &[&[&str]] = &[&["select-pane", "-t", "p:0.1"]];
const ALONE_P00: &[&[&str]] = &[&["kill-pane", "-t", "p:0.1"]];
const ZOOMED_P00: &[&[&str]] = &[&["resize-pane", "-Z", "-t", "p:0.0"]];
/// `%0` stays the current pane of `p:0`, which is no longer the active window.
const BEHIND_P1: &[&[&str]] = &[&["select-window", "-t", "p:1"]];

/// Commands answered as on a healthy server: those that name a healthy pane or
/// the window, and those naming `%0` that resize nothing.
const SERVED: &[Case] = &[
    // tmux 3.7b's reproduction.
    case(&["select-pane", "-t", "p:0.1"]),
    Case {
        before: SELECTED_P01,
        command: &["select-pane", "-t", "p:0.1"],
    },
    Case {
        before: &[
            &["select-pane", "-t", "p:0.1"],
            &["select-pane", "-t", "p:0.0"],
        ],
        command: &["last-pane", "-t", "p:0"],
    },
    case(&["kill-pane", "-t", "p:0.1"]),
    Case {
        before: SELECTED_P01,
        command: &["kill-pane", "-t", "p:0.1"],
    },
    // The same resize, after other changes of the window's layout.
    Case {
        before: SELECTED_P01,
        command: &["last-pane", "-t", "p:0"],
    },
    Case {
        before: SELECTED_P01,
        command: &["select-pane", "-U", "-t", "p:0.1"],
    },
    case(&["resize-pane", "-t", "p:0.1", "-U", "2"]),
    case(&["break-pane", "-d", "-s", "p:0.1"]),
    case(&["kill-pane", "-a", "-t", "p:0.1"]),
    case(&["select-layout", "-t", "p:0", "tiled"]),
    case(&["rotate-window", "-t", "p:0"]),
    case(&["resize-window", "-t", "p:0", "-x", "70", "-y", "20"]),
    case(&["swap-window", "-d", "-s", "p:0", "-t", "p:1"]),
    // The healthy pane leaves the window, or another pane joins it, from
    // the same session or from another one.
    case(&["join-pane", "-d", "-s", "p:0.1", "-t", "other:0.0"]),
    case(&["move-pane", "-d", "-s", "p:0.1", "-t", "p:1.0"]),
    case(&["join-pane", "-d", "-s", "p:1.0", "-t", "p:0.1"]),
    case(&["join-pane", "-d", "-s", "other:0.0", "-t", "p:0.1"]),
    case(&["move-pane", "-d", "-s", "other:0.0", "-t", "p:1.0"]),
    // Commands that name `%0` and resize nothing, served already.
    case(&["select-pane", "-t", "p:0.0"]),
    case(&["resize-pane", "-t", "p:0.0", "-x", "40"]),
    case(&["kill-pane", "-t", "p:0.0"]),
    // `%0` alone in its window: the selection changes nothing to resize.
    Case {
        before: ALONE_P00,
        command: &["select-pane", "-L", "-t", "p:0.0"],
    },
];

/// Commands that name `%0` and resize its window: refused, nothing changed.
const FAIL_FAST: &[Case] = &[
    Case {
        before: SELECTED_P01,
        command: &["select-pane", "-t", "p:0.0"],
    },
    case(&["select-pane", "-D", "-t", "p:0.0"]),
    case(&["resize-pane", "-t", "p:0.0", "-D", "2"]),
    case(&["resize-pane", "-Z", "-t", "p:0.0"]),
    case(&["break-pane", "-d", "-s", "p:0.0"]),
    Case {
        before: ALONE_P00,
        command: &["break-pane", "-d", "-s", "p:0.0"],
    },
    case(&["kill-pane", "-a", "-t", "p:0.0"]),
    // `%0` moved, or joined by another pane, resizes both windows.
    case(&["join-pane", "-d", "-s", "p:0.0", "-t", "other:0.0"]),
    case(&["move-pane", "-d", "-s", "p:0.0", "-t", "other:0.0"]),
    case(&["join-pane", "-d", "-s", "p:0.0", "-t", "p:1.0"]),
    case(&["move-pane", "-d", "-s", "p:0.0", "-t", "p:1.0"]),
    case(&["join-pane", "-d", "-s", "p:0.0", "-t", "p:0.1"]),
    case(&["join-pane", "-d", "-s", "p:1.0", "-t", "p:0.0"]),
    case(&["move-pane", "-d", "-s", "other:0.0", "-t", "p:0.0"]),
    Case {
        before: ALONE_P00,
        command: &["join-pane", "-d", "-s", "p:0.0", "-t", "other:0.0"],
    },
    Case {
        before: ALONE_P00,
        command: &["move-pane", "-d", "-s", "p:0.0", "-t", "p:1.0"],
    },
    case(&["swap-pane", "-d", "-s", "p:0.0", "-t", "p:0.1"]),
    // Each transfer naming the current pane of a window that is not active.
    Case {
        before: BEHIND_P1,
        command: &["swap-pane", "-d", "-s", "p:0.0", "-t", "p:0.1"],
    },
    Case {
        before: BEHIND_P1,
        command: &["join-pane", "-d", "-s", "p:0.0", "-t", "other:0.0"],
    },
    Case {
        before: BEHIND_P1,
        command: &["move-pane", "-d", "-s", "p:1.0", "-t", "p:0.0"],
    },
    Case {
        before: BEHIND_P1,
        command: &["break-pane", "-d", "-s", "p:0.0"],
    },
    // `%0` swapped with itself unzooms its window.
    Case {
        before: ZOOMED_P00,
        command: &["swap-pane", "-d", "-s", "p:0.0", "-t", "p:0.0"],
    },
];

/// Transfers naming `%0` that tmux 3.7b answers without moving anything,
/// success or error: each answers as on a healthy server and changes nothing.
const NOTHING_MOVED: &[Case] = &[
    case(&["swap-pane", "-d", "-s", "p:0.0", "-t", "p:0.0"]),
    Case {
        before: BEHIND_P1,
        command: &["swap-pane", "-d", "-s", "p:0.0", "-t", "p:0.0"],
    },
    Case {
        before: ZOOMED_P00,
        command: &["swap-pane", "-Z", "-d", "-s", "p:0.0", "-t", "p:0.0"],
    },
    Case {
        before: ALONE_P00,
        command: &["swap-pane", "-U", "-t", "p:0.0"],
    },
    case(&["join-pane", "-d", "-s", "p:0.0", "-t", "p:0.0"]),
    case(&["move-pane", "-d", "-s", "p:0.0", "-t", "p:0.0"]),
    case(&["break-pane", "-d", "-s", "p:0.0", "-t", "p:1"]),
    Case {
        before: BEHIND_P1,
        command: &["break-pane", "-d", "-s", "p:0.0", "-t", "p:1"],
    },
    Case {
        before: ALONE_P00,
        command: &["break-pane", "-d", "-s", "p:0.0", "-t", "p:0"],
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expected {
    Served,
    FailFast,
    NothingMoved,
}

/// Runs each case on a healthy server, then on a server where `p:0.0` is
/// poisoned after the case's setup. A served case answers as the healthy
/// server does and leaves every pane as it does; a fail-fast case answers
/// "pane %0 unavailable" and leaves every pane as the poisoned server had it;
/// a case that moves nothing answers as the healthy server does, and neither
/// server changes a pane.
async fn pane_commands_beside_a_poisoned_pane(sent: Sent, attached: bool) {
    let mut differences = Vec::new();
    for (cases, expected) in [
        (SERVED, Expected::Served),
        (FAIL_FAST, Expected::FailFast),
        (NOTHING_MOVED, Expected::NothingMoved),
    ] {
        for Case { before, command } in cases {
            if sent != Sent::Queued && typed(sent, command).is_none() {
                continue;
            }
            let mut outcomes = Vec::new();
            for poisoned in [false, true] {
                let handler = RequestHandler::new();
                let _client = window_beside_a_poisoned_pane(&handler, attached).await;
                for step in *before {
                    run(&handler, step).await;
                }
                if poisoned {
                    handler
                        .poison_pane_transcript_for_test(&pane("p:0.0"))
                        .await;
                }
                let before_command = panes(&handler).await;
                let answered = answer(&handler, sent, command).await;
                outcomes.push((before_command, answered, panes(&handler).await));
            }
            let (healthy, poisoned) = (&outcomes[0], &outcomes[1]);
            if expected == Expected::NothingMoved {
                assert_eq!(healthy.0, healthy.2, "{command:?} on a healthy server");
            } else {
                assert_eq!(
                    healthy.1,
                    Ok(String::new()),
                    "{command:?} on a healthy server"
                );
            }
            let expected = match expected {
                Expected::Served => (healthy.1.clone(), healthy.2.clone()),
                Expected::FailFast => (Err("pane %0 unavailable".to_owned()), poisoned.0.clone()),
                Expected::NothingMoved => (healthy.1.clone(), poisoned.0.clone()),
            };
            if (poisoned.1.clone(), poisoned.2.clone()) != expected {
                differences.push(format!(
                    "{command:?} after {before:?}: {:?}\n{}\nexpected {:?}\n{}",
                    poisoned.1, poisoned.2, expected.0, expected.1
                ));
            }
        }
    }
    assert_eq!(
        differences,
        Vec::<String>::new(),
        "{sent:?} attached={attached}"
    );
}

#[tokio::test]
async fn typed_pane_commands_beside_a_poisoned_pane_are_served() {
    pane_commands_beside_a_poisoned_pane(Sent::Typed, false).await;
}

#[tokio::test]
async fn queued_pane_commands_beside_a_poisoned_pane_are_served() {
    pane_commands_beside_a_poisoned_pane(Sent::Queued, false).await;
}

#[tokio::test]
async fn typed_pane_commands_seen_by_a_client_of_the_poisoned_pane_are_served() {
    pane_commands_beside_a_poisoned_pane(Sent::Typed, true).await;
}

#[tokio::test]
async fn queued_pane_commands_seen_by_a_client_of_the_poisoned_pane_are_served() {
    pane_commands_beside_a_poisoned_pane(Sent::Queued, true).await;
}

/// The SDK selects a pane by reference through its own handler, with the same
/// resize: the healthy pane is selected, the poisoned one refused.
#[tokio::test]
async fn sdk_pane_selections_beside_a_poisoned_pane_are_served() {
    pane_commands_beside_a_poisoned_pane(Sent::Sdk, false).await;
    pane_commands_beside_a_poisoned_pane(Sent::Sdk, true).await;
}
