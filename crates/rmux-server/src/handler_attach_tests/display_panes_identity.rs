use super::*;

use crate::handler::scripting_support::QueueExecutionContext;

async fn arm_display_panes(
    handler: &RequestHandler,
    requester_pid: u32,
    session: SessionName,
    target_client: Option<String>,
    template: Option<String>,
) {
    let response = handler
        .dispatch(
            requester_pid,
            Request::DisplayPanes(Box::new(rmux_proto::DisplayPanesRequest {
                target: session,
                duration_ms: Some(60_000),
                non_blocking: true,
                no_command: false,
                template,
                target_client,
            })),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::DisplayPanes(_)),
        "{response:?}"
    );
}

async fn displayed_label_for_pane(
    handler: &RequestHandler,
    attach_pid: u32,
    pane_index: u32,
) -> String {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .and_then(|active| active.display_panes.as_ref())
        .and_then(|display_panes| {
            display_panes
                .labels
                .iter()
                .find(|label| label.target.pane_index() == pane_index)
        })
        .map(|label| label.label.clone())
        .expect("requested pane has a displayed label")
}

async fn assert_probe_absent(handler: &RequestHandler, name: &str) {
    assert!(
        handler.state.lock().await.buffers.show(Some(name)).is_err(),
        "stale display-panes selection must not execute its command"
    );
}

#[tokio::test]
async fn display_panes_default_selection_rejects_a_relinked_window_occurrence() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let session = session_name("display-panes-relinked-occurrence");
    let _control_rx = create_attached_session(&handler, requester_pid, &session).await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::with_window(session.clone(), 0, 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        Response::SelectPane(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(session.clone(), 0),
                target: WindowTarget::with_window(session.clone(), 2),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(session.clone(), 2),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    arm_display_panes(&handler, requester_pid, session.clone(), None, None).await;
    let label = displayed_label_for_pane(&handler, requester_pid, 1).await;
    {
        let mut state = handler.state.lock().await;
        state
            .unlink_window(WindowTarget::with_window(session.clone(), 2), false)
            .expect("displayed occurrence unlinks");
        state
            .link_window(LinkWindowRequest {
                source: WindowTarget::with_window(session.clone(), 0),
                target: WindowTarget::with_window(session.clone(), 2),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            })
            .expect("same window relinks at the displayed slot");
    }

    let result = handler
        .handle_attached_live_input_for_test(requester_pid, label.as_bytes())
        .await;
    assert!(result.is_err(), "stale label must fail closed");
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .expect("shared window survives")
            .active_pane_index(),
        0,
        "the replacement occurrence must not receive the stale selection"
    );
}

#[tokio::test]
async fn display_panes_command_rejects_a_reused_pane_slot() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let session = session_name("display-panes-pane-slot-aba");
    let _control_rx = create_attached_session(&handler, requester_pid, &session).await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    arm_display_panes(
        &handler,
        requester_pid,
        session.clone(),
        None,
        Some("set-buffer -b display-panes-slot-aba fired".to_owned()),
    )
    .await;
    let label = displayed_label_for_pane(&handler, requester_pid, 1).await;

    assert!(matches!(
        handler
            .handle(Request::KillPane(rmux_proto::KillPaneRequest {
                target: PaneTarget::with_window(session.clone(), 0, 1),
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let result = handler
        .handle_attached_live_input_for_test(requester_pid, label.as_bytes())
        .await;
    assert!(result.is_err(), "stale label must fail closed");
    assert_probe_absent(&handler, "display-panes-slot-aba").await;
}

#[tokio::test]
async fn display_panes_command_rejects_a_respawned_pane_generation() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let session = session_name("display-panes-respawn-generation");
    let _control_rx = create_attached_session(&handler, requester_pid, &session).await;
    arm_display_panes(
        &handler,
        requester_pid,
        session.clone(),
        None,
        Some("set-buffer -b display-panes-respawn fired".to_owned()),
    )
    .await;
    let label = displayed_label_for_pane(&handler, requester_pid, 0).await;

    assert!(matches!(
        handler
            .handle(Request::RespawnPane(Box::new(
                rmux_proto::RespawnPaneRequest {
                    target: PaneTarget::with_window(session, 0, 0),
                    kill: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    process_command: None,
                }
            )))
            .await,
        Response::RespawnPane(_)
    ));

    let result = handler
        .handle_attached_live_input_for_test(requester_pid, label.as_bytes())
        .await;
    assert!(result.is_err(), "stale label must fail closed");
    assert_probe_absent(&handler, "display-panes-respawn").await;
}

#[tokio::test]
async fn display_panes_target_client_does_not_bypass_the_queued_lifecycle_lease() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let target_pid = requester_pid.saturating_add(1);
    let lifecycle_session = session_name("display-panes-lease-origin");
    let displayed_session = session_name("display-panes-lease-client");
    let _origin_rx = create_attached_session(&handler, requester_pid, &lifecycle_session).await;
    let _target_rx = create_attached_session(&handler, target_pid, &displayed_session).await;
    let lifecycle_target = PaneTarget::with_window(lifecycle_session.clone(), 0, 0);
    let lease = handler
        .state
        .lock()
        .await
        .capture_retained_pane_lifecycle_target(&lifecycle_target)
        .expect("lifecycle target is retainable");
    let commands = handler
        .parse_control_commands(&format!(
            "display-panes -b -t {target_pid} 'set-buffer -b display-panes-client-lease fired'"
        ))
        .await
        .expect("targeted display-panes parses");
    handler
        .execute_parsed_commands(
            requester_pid,
            commands,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(lifecycle_target.clone())))
                .with_retained_lifecycle_target(Some(lease)),
        )
        .await
        .expect("targeted display-panes arms while the lease is live");
    let label = displayed_label_for_pane(&handler, target_pid, 0).await;

    assert!(matches!(
        handler
            .handle(Request::RespawnPane(Box::new(
                rmux_proto::RespawnPaneRequest {
                    target: lifecycle_target,
                    kill: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    process_command: None,
                }
            )))
            .await,
        Response::RespawnPane(_)
    ));

    let result = handler
        .handle_attached_live_input_for_test(target_pid, label.as_bytes())
        .await;
    assert!(result.is_err(), "retired lease must fail closed");
    assert_probe_absent(&handler, "display-panes-client-lease").await;
}
