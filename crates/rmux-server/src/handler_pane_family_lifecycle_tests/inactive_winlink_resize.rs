use super::RequestHandler;
use crate::pane_io::AttachControl;
use rmux_proto::{
    KillPaneRequest, KillSessionRequest, LinkWindowRequest, MoveWindowRequest, MoveWindowTarget,
    NewSessionRequest, NewWindowRequest, OptionName, PaneKillRequest, PaneTarget, PaneTargetRef,
    Request, Response, ScopeSelector, SelectWindowRequest, SessionName, SetOptionMode,
    SetOptionRequest, TerminalSize, UnlinkWindowRequest, WindowTarget,
};
use tokio::sync::mpsc;

pub(super) const LARGE_SIZE: TerminalSize = TerminalSize {
    cols: 132,
    rows: 43,
};
pub(super) const SMALL_SIZE: TerminalSize = TerminalSize { cols: 72, rows: 19 };

#[derive(Clone, Copy)]
struct InactiveWinlinkScenario {
    policy: &'static str,
    shared_size: TerminalSize,
    other_size: TerminalSize,
}

#[derive(Clone, Copy)]
enum InactiveAliasRemoval {
    PaneKill,
    UnlinkWindow,
    KillSession,
}

pub(super) async fn create_sized_session(
    handler: &RequestHandler,
    name: &str,
    size: TerminalSize,
) -> SessionName {
    let session_name = SessionName::new(name).expect("valid test session name");
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(size),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session_name
}

pub(super) async fn link_window(
    handler: &RequestHandler,
    source: WindowTarget,
    target: WindowTarget,
) {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source,
            target,
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}

pub(super) async fn select_window(handler: &RequestHandler, target: WindowTarget) {
    let response = handler
        .handle(Request::SelectWindow(SelectWindowRequest { target }))
        .await;
    assert!(
        matches!(response, Response::SelectWindow(_)),
        "{response:?}"
    );
}

pub(super) async fn set_window_size_policy(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    policy: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(
                session_name.clone(),
                window_index,
            )),
            option: OptionName::WindowSize,
            value: policy.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

pub(super) async fn register_sized_attach(
    handler: &RequestHandler,
    attach_pid: u32,
    session_name: &SessionName,
    size: TerminalSize,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    handler
        .handle_attached_resize(attach_pid, size)
        .await
        .expect("attached resize succeeds");
    control_rx
}

pub(super) async fn assert_window_and_pty_size(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    expected: TerminalSize,
) {
    let master = {
        let mut state = handler.state.lock().await;
        let window = state
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .expect("window exists");
        assert_eq!(window.size(), expected, "window model size");
        state
            .clone_pane_master_if_alive(session_name, window_index, 0)
            .expect("pane PTY is alive")
    };
    let pty_size = master.size().expect("pane PTY size is readable");
    let actual = TerminalSize {
        cols: pty_size.cols,
        rows: pty_size.rows,
    };
    let without_status = TerminalSize {
        cols: expected.cols,
        rows: expected.rows.saturating_sub(1),
    };
    assert!(
        actual == expected || actual == without_status,
        "PTY follows only its active linked window: expected {expected:?}, got {actual:?}"
    );
}

async fn assert_destroyed_shared_active_window_preserves_inactive_winlink_runtime(
    label: &str,
    scenario: InactiveWinlinkScenario,
    first_pid: u32,
) {
    let handler = RequestHandler::new();
    let source =
        create_sized_session(&handler, &format!("{label}-source"), scenario.shared_size).await;
    let other =
        create_sized_session(&handler, &format!("{label}-other"), scenario.shared_size).await;

    link_window(
        &handler,
        WindowTarget::with_window(source.clone(), 0),
        WindowTarget::with_window(other.clone(), 1),
    )
    .await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: source.clone(),
            name: Some("temporary-shared-active".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    link_window(
        &handler,
        WindowTarget::with_window(source.clone(), 1),
        WindowTarget::with_window(other.clone(), 2),
    )
    .await;
    select_window(&handler, WindowTarget::with_window(source.clone(), 1)).await;
    select_window(&handler, WindowTarget::with_window(other.clone(), 2)).await;
    handler.wait_for_initial_panes_for_test().await;

    for (session_name, window_index) in [(&source, 0), (&other, 1), (&source, 1), (&other, 2)] {
        set_window_size_policy(&handler, session_name, window_index, scenario.policy).await;
    }

    let _source_rx =
        register_sized_attach(&handler, first_pid, &source, scenario.shared_size).await;
    let _other_rx =
        register_sized_attach(&handler, first_pid + 1, &other, scenario.other_size).await;
    assert_window_and_pty_size(&handler, &source, 0, scenario.shared_size).await;

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(source.clone(), 1, 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        let source_session = state.sessions.session(&source).expect("source survives");
        let other_session = state.sessions.session(&other).expect("other survives");
        assert_eq!(source_session.active_window_index(), 0);
        assert_eq!(other_session.active_window_index(), 0);
        assert!(source_session.window_at(1).is_none());
        assert!(other_session.window_at(2).is_none());
        assert_eq!(
            other_session
                .window_at(1)
                .expect("inactive linked alias survives")
                .size(),
            scenario.shared_size,
            "reconciling the other active window must not mutate its inactive alias"
        );
    }
    assert_window_and_pty_size(&handler, &source, 0, scenario.shared_size).await;
    assert_window_and_pty_size(&handler, &other, 0, scenario.other_size).await;
}

async fn assert_inactive_alias_removal_reconciles_surviving_window(
    label: &str,
    removal: InactiveAliasRemoval,
    scenario: InactiveWinlinkScenario,
    first_pid: u32,
) {
    let handler = RequestHandler::new();
    let source =
        create_sized_session(&handler, &format!("{label}-source"), scenario.shared_size).await;
    let survivor =
        create_sized_session(&handler, &format!("{label}-survivor"), scenario.shared_size).await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: source.clone(),
            name: Some("inactive-shared-window".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    link_window(
        &handler,
        WindowTarget::with_window(source.clone(), 1),
        WindowTarget::with_window(survivor.clone(), 1),
    )
    .await;
    handler.wait_for_initial_panes_for_test().await;

    for session_name in [&source, &survivor] {
        set_window_size_policy(&handler, session_name, 0, "manual").await;
        set_window_size_policy(&handler, session_name, 1, scenario.policy).await;
    }
    let _source_rx =
        register_sized_attach(&handler, first_pid, &source, scenario.shared_size).await;
    let _survivor_rx =
        register_sized_attach(&handler, first_pid + 1, &survivor, scenario.other_size).await;
    assert_window_and_pty_size(&handler, &source, 1, scenario.shared_size).await;
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&source)
                .expect("source exists")
                .active_window_index(),
            0
        );
        assert_eq!(
            state
                .sessions
                .session(&survivor)
                .expect("survivor exists")
                .active_window_index(),
            0
        );
    }

    let response = match removal {
        InactiveAliasRemoval::PaneKill => {
            handler
                .handle(Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::slot(PaneTarget::with_window(source.clone(), 1, 0)),
                    kill_all_except: false,
                }))
                .await
        }
        InactiveAliasRemoval::UnlinkWindow => {
            handler
                .handle(Request::UnlinkWindow(UnlinkWindowRequest {
                    target: WindowTarget::with_window(source.clone(), 1),
                    kill_if_last: false,
                }))
                .await
        }
        InactiveAliasRemoval::KillSession => {
            handler
                .handle(Request::KillSession(KillSessionRequest {
                    target: source.clone(),
                    kill_all_except_target: false,
                    clear_alerts: false,
                    kill_group: false,
                }))
                .await
        }
    };
    assert!(
        matches!(
            response,
            Response::KillPane(_) | Response::UnlinkWindow(_) | Response::KillSession(_)
        ),
        "{response:?}"
    );

    {
        let state = handler.state.lock().await;
        assert!(state
            .sessions
            .session(&source)
            .and_then(|session| session.window_at(1))
            .is_none());
        let survivor_session = state.sessions.session(&survivor).expect("survivor exists");
        assert_eq!(survivor_session.active_window_index(), 0);
        assert!(survivor_session.window_at(1).is_some());
    }
    assert_window_and_pty_size(&handler, &survivor, 1, scenario.other_size).await;
}

async fn assert_link_window_reconciles_new_inactive_alias(
    label: &str,
    scenario: InactiveWinlinkScenario,
    first_pid: u32,
) {
    let handler = RequestHandler::new();
    let source =
        create_sized_session(&handler, &format!("{label}-source"), scenario.shared_size).await;
    let target =
        create_sized_session(&handler, &format!("{label}-target"), scenario.other_size).await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: source.clone(),
            name: Some("linked-after-attach".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;
    for session_name in [&source, &target] {
        set_window_size_policy(&handler, session_name, 0, "manual").await;
    }
    set_window_size_policy(&handler, &source, 1, scenario.policy).await;
    let _source_rx =
        register_sized_attach(&handler, first_pid, &source, scenario.shared_size).await;
    let _target_rx =
        register_sized_attach(&handler, first_pid + 1, &target, scenario.other_size).await;
    assert_window_and_pty_size(&handler, &source, 1, scenario.shared_size).await;

    link_window(
        &handler,
        WindowTarget::with_window(source.clone(), 1),
        WindowTarget::with_window(target.clone(), 1),
    )
    .await;

    assert_window_and_pty_size(&handler, &source, 1, scenario.other_size).await;
    assert_window_and_pty_size(&handler, &target, 1, scenario.other_size).await;
}

async fn assert_move_window_reconciles_changed_inactive_alias_family(
    label: &str,
    scenario: InactiveWinlinkScenario,
    first_pid: u32,
) {
    let handler = RequestHandler::new();
    let source =
        create_sized_session(&handler, &format!("{label}-source"), scenario.shared_size).await;
    let peer = create_sized_session(&handler, &format!("{label}-peer"), scenario.other_size).await;
    let destination = create_sized_session(
        &handler,
        &format!("{label}-destination"),
        scenario.other_size,
    )
    .await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: source.clone(),
            name: Some("moved-after-attach".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    link_window(
        &handler,
        WindowTarget::with_window(source.clone(), 1),
        WindowTarget::with_window(peer.clone(), 1),
    )
    .await;
    handler.wait_for_initial_panes_for_test().await;
    for session_name in [&source, &peer, &destination] {
        set_window_size_policy(&handler, session_name, 0, "manual").await;
    }
    set_window_size_policy(&handler, &source, 1, scenario.policy).await;
    let _source_rx =
        register_sized_attach(&handler, first_pid, &source, scenario.shared_size).await;
    let _peer_rx = register_sized_attach(&handler, first_pid + 1, &peer, scenario.other_size).await;
    let _destination_rx =
        register_sized_attach(&handler, first_pid + 2, &destination, scenario.other_size).await;
    assert_window_and_pty_size(&handler, &source, 1, scenario.shared_size).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(destination.clone(), 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_window_and_pty_size(&handler, &peer, 1, scenario.other_size).await;
    assert_window_and_pty_size(&handler, &destination, 1, scenario.other_size).await;
}

#[tokio::test]
async fn smallest_fanout_does_not_resize_a_session_wide_inactive_winlink() {
    assert_destroyed_shared_active_window_preserves_inactive_winlink_runtime(
        "inactive-winlink-smallest",
        InactiveWinlinkScenario {
            policy: "smallest",
            shared_size: SMALL_SIZE,
            other_size: LARGE_SIZE,
        },
        7411,
    )
    .await;
}

#[tokio::test]
async fn largest_fanout_does_not_resize_a_session_wide_inactive_winlink() {
    assert_destroyed_shared_active_window_preserves_inactive_winlink_runtime(
        "inactive-winlink-largest",
        InactiveWinlinkScenario {
            policy: "largest",
            shared_size: LARGE_SIZE,
            other_size: SMALL_SIZE,
        },
        7421,
    )
    .await;
}

#[tokio::test]
async fn pane_kill_reconciles_inactive_smallest_winlink_survivor() {
    assert_inactive_alias_removal_reconciles_surviving_window(
        "inactive-pane-kill-smallest",
        InactiveAliasRemoval::PaneKill,
        InactiveWinlinkScenario {
            policy: "smallest",
            shared_size: SMALL_SIZE,
            other_size: LARGE_SIZE,
        },
        7431,
    )
    .await;
}

#[tokio::test]
async fn pane_kill_reconciles_inactive_largest_winlink_survivor() {
    assert_inactive_alias_removal_reconciles_surviving_window(
        "inactive-pane-kill-largest",
        InactiveAliasRemoval::PaneKill,
        InactiveWinlinkScenario {
            policy: "largest",
            shared_size: LARGE_SIZE,
            other_size: SMALL_SIZE,
        },
        7441,
    )
    .await;
}

#[tokio::test]
async fn unlink_window_reconciles_inactive_smallest_winlink_survivor() {
    assert_inactive_alias_removal_reconciles_surviving_window(
        "inactive-unlink-smallest",
        InactiveAliasRemoval::UnlinkWindow,
        InactiveWinlinkScenario {
            policy: "smallest",
            shared_size: SMALL_SIZE,
            other_size: LARGE_SIZE,
        },
        7451,
    )
    .await;
}

#[tokio::test]
async fn unlink_window_reconciles_inactive_largest_winlink_survivor() {
    assert_inactive_alias_removal_reconciles_surviving_window(
        "inactive-unlink-largest",
        InactiveAliasRemoval::UnlinkWindow,
        InactiveWinlinkScenario {
            policy: "largest",
            shared_size: LARGE_SIZE,
            other_size: SMALL_SIZE,
        },
        7461,
    )
    .await;
}

#[tokio::test]
async fn kill_session_reconciles_inactive_smallest_winlink_survivor() {
    assert_inactive_alias_removal_reconciles_surviving_window(
        "inactive-kill-session-smallest",
        InactiveAliasRemoval::KillSession,
        InactiveWinlinkScenario {
            policy: "smallest",
            shared_size: SMALL_SIZE,
            other_size: LARGE_SIZE,
        },
        7471,
    )
    .await;
}

#[tokio::test]
async fn kill_session_reconciles_inactive_largest_winlink_survivor() {
    assert_inactive_alias_removal_reconciles_surviving_window(
        "inactive-kill-session-largest",
        InactiveAliasRemoval::KillSession,
        InactiveWinlinkScenario {
            policy: "largest",
            shared_size: LARGE_SIZE,
            other_size: SMALL_SIZE,
        },
        7481,
    )
    .await;
}

#[tokio::test]
async fn link_window_reconciles_new_inactive_smallest_alias() {
    assert_link_window_reconciles_new_inactive_alias(
        "inactive-link-smallest",
        InactiveWinlinkScenario {
            policy: "smallest",
            shared_size: LARGE_SIZE,
            other_size: SMALL_SIZE,
        },
        7491,
    )
    .await;
}

#[tokio::test]
async fn link_window_reconciles_new_inactive_largest_alias() {
    assert_link_window_reconciles_new_inactive_alias(
        "inactive-link-largest",
        InactiveWinlinkScenario {
            policy: "largest",
            shared_size: SMALL_SIZE,
            other_size: LARGE_SIZE,
        },
        7501,
    )
    .await;
}

#[tokio::test]
async fn move_window_reconciles_changed_inactive_smallest_alias_family() {
    assert_move_window_reconciles_changed_inactive_alias_family(
        "inactive-move-smallest",
        InactiveWinlinkScenario {
            policy: "smallest",
            shared_size: SMALL_SIZE,
            other_size: LARGE_SIZE,
        },
        7521,
    )
    .await;
}

#[tokio::test]
async fn move_window_reconciles_changed_inactive_largest_alias_family() {
    assert_move_window_reconciles_changed_inactive_alias_family(
        "inactive-move-largest",
        InactiveWinlinkScenario {
            policy: "largest",
            shared_size: LARGE_SIZE,
            other_size: SMALL_SIZE,
        },
        7531,
    )
    .await;
}
