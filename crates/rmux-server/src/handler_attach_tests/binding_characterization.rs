use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;

async fn bind(handler: &RequestHandler, command: &str) {
    bind_attached_prompt_test_key(handler, "X", vec![command.to_owned()]).await;
}
async fn option(handler: &RequestHandler, session: &SessionName, name: &str) -> String {
    let response = handler
        .handle(Request::ShowOptions(rmux_proto::ShowOptionsRequest {
            scope: rmux_proto::OptionScopeSelector::Session(session.clone()),
            name: Some(name.to_owned()),
            value_only: true,
            include_inherited: false,
            quiet: true,
            include_hooks: false,
        }))
        .await;
    let Response::ShowOptions(response) = response else {
        panic!("{response:?}");
    };
    String::from_utf8(response.output.stdout().to_vec()).unwrap()
}
async fn kill(handler: &RequestHandler, name: &SessionName) {
    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
}
fn detach_events(
    events: &mut tokio::sync::broadcast::Receiver<crate::handler::QueuedLifecycleEvent>,
) -> Vec<crate::handler::QueuedLifecycleEvent> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| {
            matches!(
                event.event,
                rmux_core::LifecycleEvent::ClientDetached { .. }
            )
        })
        .collect()
}

#[tokio::test]
async fn l05_1_binding_detach_discards_tail_after_registration_reuse() {
    let handler = RequestHandler::new();
    let session = session_name("binding-detach-reuse");
    let pid = 978_101;
    let mut old_events = create_attached_session(&handler, pid, &session).await;
    let old_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .unwrap()
        .id();
    bind(&handler, "detach-client ; set-option @tail forbidden").await;
    let pause = handler.install_attached_size_selection_pause();
    let mut events = handler.subscribe_lifecycle_events();
    let executing = handler.clone();
    let binding = tokio::spawn(async move {
        executing
            .handle_attached_live_input_for_test(pid, b"\x02X")
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .unwrap();
    recv_matching_attach_control(&mut old_events, "committed detach", |event| {
        matches!(event, AttachControl::Detach)
    })
    .await;
    kill(&handler, &session).await;
    let _replacement_events = create_attached_session(&handler, pid, &session).await;
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .unwrap()
        .id();
    assert_ne!(replacement_id, old_session_id);
    pause.release.notify_one();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, binding)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(option(&handler, &session, "@tail").await, "");
    assert_eq!(
        handler
            .active_attach_identity_for_test(pid)
            .await
            .session_id(),
        replacement_id
    );
    let detached = detach_events(&mut events);
    assert_eq!(
        detached.len(),
        1,
        "one old detach across name and registration reuse: {detached:?}"
    );
    assert!(
        detached[0]
            .formats
            .contains(&("hook_session".to_owned(), old_session_id.to_string())),
        "detach formats must retain the old session: {:?}",
        detached[0]
    );
}

#[tokio::test]
async fn l05_1_binding_rename_success_failure_tail() {
    for collision in [false, true] {
        let handler = RequestHandler::new();
        let original = session_name("binding-rename-source");
        let destination = session_name("binding-rename-destination");
        let pid = 978_102;
        let _events = create_attached_session(&handler, pid, &original).await;
        if collision {
            let response = handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: destination.clone(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await;
            crate::session_fixture::assert_new_session(&response);
        }
        let identity = handler
            .active_attach_identity_for_test(pid)
            .await
            .session_id();
        bind(&handler, &format!("rename-session {destination} ; set-option @tail committed\nset-option -F @observation '#{{session_name}}'")).await;
        handler
            .handle_attached_live_input_for_test(pid, b"\x02X")
            .await
            .unwrap();
        let current = if collision { &original } else { &destination };
        assert_eq!(
            handler
                .active_attach_identity_for_test(pid)
                .await
                .session_id(),
            identity
        );
        assert_eq!(
            option(&handler, current, "@tail").await,
            if collision { "" } else { "committed\n" }
        );
        assert_eq!(
            option(&handler, current, "@observation").await,
            format!("{current}\n")
        );
        if collision {
            assert_eq!(option(&handler, &destination, "@observation").await, "");
        } else {
            let response = handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: original.clone(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await;
            crate::session_fixture::assert_new_session(&response);
            assert_eq!(option(&handler, &original, "@tail").await, "");
        }
    }
}

#[cfg(feature = "web")]
#[tokio::test]
async fn l05_1_binding_destroy_tail_name_reuse() {
    let handler = RequestHandler::new();
    let session = session_name("binding-destroy-reuse");
    let pid = 978_103;
    let _events = create_attached_session(&handler, pid, &session).await;
    let old_id = handler
        .active_attach_identity_for_test(pid)
        .await
        .session_id();
    bind(&handler, "kill-session ; set-option @tail forbidden").await;
    let pause = handler.install_kill_session_web_prune_pause(session.clone());
    let mut events = handler.subscribe_lifecycle_events();
    let executing = handler.clone();
    let binding = tokio::spawn(async move {
        executing
            .handle_attached_live_input_for_test(pid, b"\x02X")
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .unwrap();
    assert!(!handler
        .state
        .lock()
        .await
        .sessions
        .contains_session(&session));
    let _replacement = create_attached_session(&handler, pid, &session).await;
    let new_id = handler
        .active_attach_identity_for_test(pid)
        .await
        .session_id();
    assert_ne!(new_id, old_id);
    pause.release.notify_one();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, binding)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(option(&handler, &session, "@tail").await, "");
    assert_eq!(
        handler
            .active_attach_identity_for_test(pid)
            .await
            .session_id(),
        new_id
    );
    let closed = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, rmux_core::LifecycleEvent::SessionClosed { .. }))
        .collect::<Vec<_>>();
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0].event.session_id(), Some(old_id.as_u32()));
}

#[tokio::test]
async fn l05_1_binding_select_then_remove_focus_destination() {
    let handler = RequestHandler::new();
    let session = session_name("binding-select-remove");
    let pid = 978_104;
    let _events = create_attached_session(&handler, pid, &session).await;
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)));
    handler.wait_for_initial_panes_for_test().await;
    let survivor = PaneTarget::with_window(session.clone(), 0, 0);
    let selected = handler
        .handle(Request::PaneSelect(rmux_proto::PaneSelectRequest {
            target: rmux_proto::PaneTargetRef::Slot(survivor.clone()),
            title: None,
        }))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    handler.disable_pane_focus_reports_for_test(&survivor).await;
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&survivor);
    bind(
        &handler,
        &format!("select-pane -t {session}:0.1 ; send-keys -l TAIL"),
    )
    .await;
    let pause = install_queue_exact_target_capture_pause(&handler, "send-keys");
    let executing = handler.clone();
    let binding = tokio::spawn(async move {
        executing
            .handle_attached_live_input_for_test(pid, b"\x02X")
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
        .await
        .unwrap();
    let response = handler
        .handle(Request::KillPane(rmux_proto::KillPaneRequest {
            target: PaneTarget::with_window(session.clone(), 0, 1),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)));
    pause.release.notify_one();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, binding)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(&survivor),
        Some(Vec::new())
    );
}
