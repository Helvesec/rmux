use super::{create_grouped_last_pane_family, RequestHandler};
use crate::pane_io::{AttachControl, PaneExitEvent};
use rmux_proto::{
    KillPaneRequest, LinkWindowRequest, NewSessionRequest, NewWindowRequest, OptionName,
    PaneKillRequest, PaneTarget, PaneTargetRef, Request, Response, ScopeSelector,
    SelectWindowRequest, SessionName, SetOptionMode, SetOptionRequest, TerminalSize,
    UnlinkWindowRequest, WindowTarget,
};
use tokio::sync::mpsc;

const LARGE_SIZE: TerminalSize = TerminalSize {
    cols: 132,
    rows: 43,
};
const SMALL_SIZE: TerminalSize = TerminalSize { cols: 72, rows: 19 };

#[derive(Clone, Copy)]
enum AliasRemoval {
    PaneKill,
    UnlinkWindow,
}

#[derive(Clone, Copy)]
enum LinkedFamilyRemoval {
    KillPane,
    NaturalExit,
}

#[derive(Clone, Copy)]
struct ResizeScenario {
    policy: &'static str,
    source_size: TerminalSize,
    survivor_size: TerminalSize,
    expected_before: TerminalSize,
    expected_after: TerminalSize,
}

const REMOVE_SMALL_CLIENT: ResizeScenario = ResizeScenario {
    policy: "smallest",
    source_size: SMALL_SIZE,
    survivor_size: LARGE_SIZE,
    expected_before: SMALL_SIZE,
    expected_after: LARGE_SIZE,
};

const REMOVE_LARGE_CLIENT: ResizeScenario = ResizeScenario {
    policy: "largest",
    source_size: LARGE_SIZE,
    survivor_size: SMALL_SIZE,
    expected_before: LARGE_SIZE,
    expected_after: SMALL_SIZE,
};

async fn create_sized_session(
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

async fn set_window_size_policy(
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

async fn register_sized_attach(
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

async fn assert_window_and_pty_size(
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
            .expect("surviving window exists");
        assert_eq!(window.size(), expected, "surviving model size");
        state
            .clone_pane_master_if_alive(session_name, window_index, 0)
            .expect("surviving pane PTY is alive")
    };
    let pty_size = master.size().expect("surviving pane PTY size is readable");
    let actual_pty_size = TerminalSize {
        cols: pty_size.cols,
        rows: pty_size.rows,
    };
    let expected_with_status_row = TerminalSize {
        cols: expected.cols,
        rows: expected.rows.saturating_sub(1),
    };
    assert!(
        actual_pty_size == expected || actual_pty_size == expected_with_status_row,
        "surviving PTY follows the reconciled window, with at most one status row: {actual_pty_size:?}",
    );
}

async fn assert_attach_removed(handler: &RequestHandler, removed_pid: u32, retained_pid: u32) {
    let active_attach = handler.active_attach.lock().await;
    assert!(!active_attach.by_pid.contains_key(&removed_pid));
    assert!(active_attach.by_pid.contains_key(&retained_pid));
}

#[tokio::test]
async fn pane_id_non_owner_alias_kill_reconciles_smallest_after_small_client_removal() {
    let handler = RequestHandler::new();
    let (_keeper, owner, peer, family_pane_id) =
        create_grouped_last_pane_family(&handler, "pane-id-smallest-non-owner").await;
    set_window_size_policy(&handler, &owner, 0, "smallest").await;

    let large_pid = 7301;
    let small_pid = 7302;
    let _large_rx = register_sized_attach(&handler, large_pid, &owner, LARGE_SIZE).await;
    let _small_rx = register_sized_attach(&handler, small_pid, &peer, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &owner, 0, SMALL_SIZE).await;

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(peer.clone(), family_pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(&peer).is_none());
        assert!(state.sessions.session(&owner).is_some());
    }
    assert_attach_removed(&handler, small_pid, large_pid).await;
    assert_window_and_pty_size(&handler, &owner, 0, LARGE_SIZE).await;
}

#[tokio::test]
async fn pane_id_runtime_owner_kill_transfers_and_reconciles_smallest_runtime() {
    let handler = RequestHandler::new();
    let (_keeper, owner, peer, family_pane_id) =
        create_grouped_last_pane_family(&handler, "pane-id-smallest-owner").await;
    set_window_size_policy(&handler, &owner, 0, "smallest").await;

    let small_pid = 7311;
    let large_pid = 7312;
    let _small_rx = register_sized_attach(&handler, small_pid, &owner, SMALL_SIZE).await;
    let _large_rx = register_sized_attach(&handler, large_pid, &peer, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &owner, 0, SMALL_SIZE).await;

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(owner.clone(), family_pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(&owner).is_none());
        assert!(state.sessions.session(&peer).is_some());
        assert_eq!(state.sessions.runtime_owner(&peer), Some(peer.clone()));
    }
    assert_attach_removed(&handler, small_pid, large_pid).await;
    assert_window_and_pty_size(&handler, &peer, 0, LARGE_SIZE).await;
}

#[tokio::test]
async fn pane_id_group_and_real_winlink_reconcile_smallest_after_alias_removal() {
    let handler = RequestHandler::new();
    let (_keeper, owner, peer, family_pane_id) =
        create_grouped_last_pane_family(&handler, "pane-id-smallest-linked").await;
    let linked_survivor = super::create_session(&handler, "pane-id-smallest-linked-survivor").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(linked_survivor.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let selected = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(linked_survivor.clone(), 1),
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectWindow(_)),
        "{selected:?}"
    );
    handler.wait_for_initial_panes_for_test().await;

    let large_pid = 7321;
    let small_pid = 7322;
    let _large_rx = register_sized_attach(&handler, large_pid, &owner, LARGE_SIZE).await;
    let _small_rx = register_sized_attach(&handler, small_pid, &peer, SMALL_SIZE).await;
    set_window_size_policy(&handler, &owner, 0, "smallest").await;
    set_window_size_policy(&handler, &peer, 0, "smallest").await;
    set_window_size_policy(&handler, &linked_survivor, 1, "smallest").await;
    handler
        .reconcile_attached_session_size_and_emit(&owner)
        .await
        .expect("linked family smallest policy reconciles");
    assert_window_and_pty_size(&handler, &owner, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &linked_survivor, 1, SMALL_SIZE).await;

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(peer.clone(), family_pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(&peer).is_none());
        assert!(state.sessions.session(&owner).is_some());
        assert!(state.sessions.session(&linked_survivor).is_some());
    }
    assert_attach_removed(&handler, small_pid, large_pid).await;
    assert_window_and_pty_size(&handler, &owner, 0, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &linked_survivor, 1, LARGE_SIZE).await;
}

async fn assert_multi_window_alias_removal_reconciles_real_winlink_survivor(
    label: &str,
    removal: AliasRemoval,
    scenario: ResizeScenario,
    first_pid: u32,
) {
    let handler = RequestHandler::new();
    let source = super::create_session(&handler, &format!("{label}-source")).await;
    let survivor = super::create_session(&handler, &format!("{label}-survivor")).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(survivor.clone(), 0),
            target: WindowTarget::with_window(source.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let selected = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(source.clone(), 1),
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectWindow(_)),
        "{selected:?}"
    );
    handler.wait_for_initial_panes_for_test().await;

    let family_pane_id = {
        let state = handler.state.lock().await;
        let source_session = state.sessions.session(&source).expect("source exists");
        assert_eq!(source_session.active_window_index(), 1);
        let linked_sessions = state.window_linked_sessions_list(&source, 1);
        assert!(linked_sessions.contains(&source));
        assert!(linked_sessions.contains(&survivor));
        source_session
            .window_at(1)
            .and_then(|window| window.pane(0))
            .expect("linked source pane exists")
            .id()
    };
    set_window_size_policy(&handler, &source, 1, scenario.policy).await;
    set_window_size_policy(&handler, &survivor, 0, scenario.policy).await;

    let survivor_pid = first_pid;
    let source_pid = first_pid + 1;
    let _survivor_rx =
        register_sized_attach(&handler, survivor_pid, &survivor, scenario.survivor_size).await;
    let _source_rx =
        register_sized_attach(&handler, source_pid, &source, scenario.source_size).await;
    handler
        .reconcile_attached_session_size_and_emit(&survivor)
        .await
        .expect("linked family size policy reconciles");
    assert_window_and_pty_size(&handler, &survivor, 0, scenario.expected_before).await;

    let response = match removal {
        AliasRemoval::PaneKill => {
            handler
                .handle(Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::by_id(source.clone(), family_pane_id),
                    kill_all_except: false,
                }))
                .await
        }
        AliasRemoval::UnlinkWindow => {
            handler
                .handle(Request::UnlinkWindow(UnlinkWindowRequest {
                    target: WindowTarget::with_window(source.clone(), 1),
                    kill_if_last: false,
                }))
                .await
        }
    };
    assert!(
        matches!(response, Response::KillPane(_) | Response::UnlinkWindow(_)),
        "{response:?}"
    );

    {
        let state = handler.state.lock().await;
        let source_session = state.sessions.session(&source).expect("source survives");
        assert!(source_session.window_at(0).is_some());
        assert!(source_session.window_at(1).is_none());
        assert!(state
            .sessions
            .session(&survivor)
            .and_then(|session| session.window_at(0))
            .is_some());
    }
    {
        let active_attach = handler.active_attach.lock().await;
        assert!(active_attach.by_pid.contains_key(&source_pid));
        assert!(active_attach.by_pid.contains_key(&survivor_pid));
    }
    assert_window_and_pty_size(&handler, &survivor, 0, scenario.expected_after).await;
}

#[tokio::test]
async fn pane_id_multi_window_alias_kill_reconciles_smallest_real_winlink_survivor() {
    assert_multi_window_alias_removal_reconciles_real_winlink_survivor(
        "pane-id-multi-window-smallest",
        AliasRemoval::PaneKill,
        REMOVE_SMALL_CLIENT,
        7331,
    )
    .await;
}

#[tokio::test]
async fn pane_id_multi_window_alias_kill_reconciles_largest_real_winlink_survivor() {
    assert_multi_window_alias_removal_reconciles_real_winlink_survivor(
        "pane-id-multi-window-largest",
        AliasRemoval::PaneKill,
        REMOVE_LARGE_CLIENT,
        7341,
    )
    .await;
}

#[tokio::test]
async fn unlink_window_reconciles_smallest_real_winlink_survivor() {
    assert_multi_window_alias_removal_reconciles_real_winlink_survivor(
        "unlink-window-smallest",
        AliasRemoval::UnlinkWindow,
        REMOVE_SMALL_CLIENT,
        7351,
    )
    .await;
}

#[tokio::test]
async fn unlink_window_reconciles_largest_real_winlink_survivor() {
    assert_multi_window_alias_removal_reconciles_real_winlink_survivor(
        "unlink-window-largest",
        AliasRemoval::UnlinkWindow,
        REMOVE_LARGE_CLIENT,
        7361,
    )
    .await;
}

async fn assert_linked_family_removal_reconciles_replacement_window(
    label: &str,
    removal: LinkedFamilyRemoval,
    scenario: ResizeScenario,
    first_pid: u32,
) {
    let handler = RequestHandler::new();
    let source = create_sized_session(
        &handler,
        &format!("{label}-source"),
        scenario.expected_before,
    )
    .await;
    let survivor = create_sized_session(
        &handler,
        &format!("{label}-survivor"),
        scenario.expected_before,
    )
    .await;
    let replacement_alias = create_sized_session(
        &handler,
        &format!("{label}-replacement-alias"),
        scenario.expected_before,
    )
    .await;
    let linked_replacement = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(survivor.clone(), 0),
            target: WindowTarget::with_window(replacement_alias.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(
        matches!(linked_replacement, Response::LinkWindow(_)),
        "{linked_replacement:?}"
    );
    let selected_replacement = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(replacement_alias.clone(), 1),
        }))
        .await;
    assert!(
        matches!(selected_replacement, Response::SelectWindow(_)),
        "{selected_replacement:?}"
    );
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: source.clone(),
            name: Some("shared".to_owned()),
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
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 1),
            target: WindowTarget::with_window(survivor.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    for session_name in [&source, &survivor] {
        let selected = handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(session_name.clone(), 1),
            }))
            .await;
        assert!(
            matches!(selected, Response::SelectWindow(_)),
            "{selected:?}"
        );
        set_window_size_policy(&handler, session_name, 1, scenario.policy).await;
    }
    set_window_size_policy(&handler, &survivor, 0, scenario.policy).await;
    set_window_size_policy(&handler, &replacement_alias, 1, scenario.policy).await;
    handler.wait_for_initial_panes_for_test().await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&source)
            .and_then(|session| session.window_at(1))
            .and_then(|window| window.pane(0))
            .expect("shared pane exists")
            .id()
    };

    let survivor_pid = first_pid;
    let source_pid = first_pid + 1;
    let _survivor_rx =
        register_sized_attach(&handler, survivor_pid, &survivor, scenario.survivor_size).await;
    let _source_rx =
        register_sized_attach(&handler, source_pid, &source, scenario.source_size).await;
    handler
        .reconcile_attached_session_size_and_emit(&survivor)
        .await
        .expect("linked family size policy reconciles");
    assert_window_and_pty_size(&handler, &survivor, 1, scenario.expected_before).await;
    assert_window_and_pty_size(&handler, &replacement_alias, 1, scenario.expected_before).await;

    match removal {
        LinkedFamilyRemoval::KillPane => {
            let response = handler
                .handle(Request::KillPane(KillPaneRequest {
                    target: PaneTarget::with_window(source.clone(), 1, 0),
                    kill_all_except: false,
                }))
                .await;
            assert!(matches!(response, Response::KillPane(_)), "{response:?}");
        }
        LinkedFamilyRemoval::NaturalExit => {
            {
                let mut state = handler.state.lock().await;
                state
                    .mark_pane_dead_without_exit_details(&PaneTarget::with_window(
                        source.clone(),
                        1,
                        0,
                    ))
                    .expect("mark linked pane naturally exited");
            }
            handler
                .handle_pane_exit_event(PaneExitEvent::eof_published(source.clone(), pane_id, None))
                .await;
        }
    }

    {
        let state = handler.state.lock().await;
        for session_name in [&source, &survivor] {
            let session = state
                .sessions
                .session(session_name)
                .expect("session survives linked family removal");
            assert!(session.window_at(0).is_some());
            assert!(
                session.window_at(1).is_none(),
                "shared window remains in {session_name}"
            );
        }
    }
    assert_window_and_pty_size(&handler, &survivor, 0, scenario.expected_after).await;
    assert_window_and_pty_size(&handler, &replacement_alias, 1, scenario.expected_after).await;
}

#[tokio::test]
async fn kill_pane_linked_family_reconciles_smallest_replacement_window() {
    assert_linked_family_removal_reconciles_replacement_window(
        "kill-pane-linked-family-smallest",
        LinkedFamilyRemoval::KillPane,
        REMOVE_SMALL_CLIENT,
        7371,
    )
    .await;
}

#[tokio::test]
async fn natural_linked_pane_exit_reconciles_largest_replacement_window() {
    assert_linked_family_removal_reconciles_replacement_window(
        "natural-linked-family-largest",
        LinkedFamilyRemoval::NaturalExit,
        REMOVE_LARGE_CLIENT,
        7381,
    )
    .await;
}
