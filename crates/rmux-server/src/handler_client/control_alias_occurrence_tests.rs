use super::*;

async fn assert_deferred_alias_flags(explicit_publish: bool) {
    let handler = RequestHandler::new();
    let session = session_name("deferred-alias");
    create_session(&handler, session.clone(), TARGET_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;
    let response = handler
        .handle(Request::LinkWindow(rmux_proto::LinkWindowRequest {
            source: WindowTarget::with_window(session.clone(), 0),
            target: WindowTarget::with_window(session.clone(), 7),
            after: false,
            before: false,
            kill_destination: false,
            detached: false,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    let window_id = active_window_id(&handler, &session).await;
    let (_, mut events) = register_control_client_with_id(&handler, 92_990, &session).await;
    settle_control_notifications(&mut events).await;
    let mut outcome = handler
        .dispatch(
            92_991,
            Request::AttachSessionExt2(Box::new(rmux_proto::request::AttachSessionExt2Request {
                target: Some(session.clone()),
                target_spec: None,
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: true,
                flags: None,
                working_directory: None,
                client_terminal: Default::default(),
                client_size: Some(TerminalSize {
                    cols: 101,
                    rows: 41,
                }),
            })),
        )
        .await;
    assert!(matches!(outcome.response, Response::AttachSession(_)));
    assert!(outcome
        .attach
        .as_ref()
        .unwrap()
        .resize_publication
        .is_some());
    {
        let mut state = handler.state.lock().await;
        let view = state.sessions.session(&session).unwrap();
        assert_eq!(view.active_window_index(), 7);
        assert_eq!(view.last_window_index(), Some(0));
        assert_eq!(
            view.window_at(0).unwrap().id(),
            view.window_at(7).unwrap().id()
        );
        state
            .sessions
            .add_winlink_alert_flags(
                &WindowTarget::with_window(session.clone(), 0),
                rmux_core::WINLINK_BELL,
            )
            .unwrap();
        state
            .sessions
            .add_winlink_alert_flags(
                &WindowTarget::with_window(session.clone(), 7),
                rmux_core::WINLINK_ACTIVITY,
            )
            .unwrap();
    }
    let before = settle_control_notifications(&mut events).await;
    assert!(
        !before
            .iter()
            .any(|line| line.starts_with("%layout-change ")),
        "{before:?}"
    );
    if explicit_publish {
        outcome
            .attach
            .as_mut()
            .unwrap()
            .resize_publication
            .take()
            .unwrap()
            .publish()
            .await;
    }
    drop(outcome);
    let prefix = format!("%layout-change @{window_id} ");
    let mut lines = collect_control_notifications_through(&mut events, &prefix).await;
    lines.extend(settle_control_notifications(&mut events).await);
    let layouts: Vec<_> = lines
        .iter()
        .filter(|line| line.starts_with(&prefix))
        .collect();
    assert_eq!(layouts.len(), 1, "{lines:?}");
    assert_eq!(
        layout_change_geometry(layouts[0]).as_deref(),
        Some("101x40")
    );
    assert_eq!(
        layouts[0].split_whitespace().last(),
        Some("#*"),
        "{lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("%client-session-changed ")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn deferred_alias_occurrence_explicit_publish_preserves_flags() {
    assert_deferred_alias_flags(true).await;
}

#[tokio::test]
async fn deferred_alias_occurrence_drop_preserves_flags() {
    assert_deferred_alias_flags(false).await;
}

#[tokio::test]
async fn deferred_alias_occurrence_notification_identity_matrix() {
    use crate::control_notifications::{collect_control_notifications, ControlClientSnapshot};
    use rmux_core::LifecycleEvent;
    for case in [
        "unchanged",
        "rename-reuse",
        "unique-reindex",
        "ambiguous-reindex",
        "slot-replacement",
        "ambiguous-replacement",
        "window-deletion",
        "session-replacement",
        "ordinary",
    ] {
        let handler = RequestHandler::new();
        let name = session_name("notification-alias");
        let renamed = session_name("notification-renamed");
        let mut state = handler.state.lock().await;
        let sid = state
            .sessions
            .create_session(name.clone(), TARGET_SIZE)
            .unwrap();
        let wid = state
            .sessions
            .session(&name)
            .unwrap()
            .window_at(0)
            .unwrap()
            .id();
        let zero = WindowTarget::with_window(name.clone(), 0);
        let target = WindowTarget::with_window(name.clone(), 7);
        let _ = state
            .sessions
            .install_window_alias(&target, wid, false, true)
            .unwrap();
        state
            .sessions
            .add_winlink_alert_flags(&zero, rmux_core::WINLINK_BELL)
            .unwrap();
        state
            .sessions
            .add_winlink_alert_flags(&target, rmux_core::WINLINK_ACTIVITY)
            .unwrap();
        let mut owner = name.clone();
        let expected = match case {
            "unchanged" | "ordinary" => Some("#*"),
            "rename-reuse" => {
                state
                    .sessions
                    .rename_session(&name, renamed.clone())
                    .unwrap();
                state
                    .sessions
                    .create_session(name.clone(), TARGET_SIZE)
                    .unwrap();
                owner = renamed;
                Some("#*")
            }
            "unique-reindex" => {
                let _ = state.sessions.remove_window(&zero).unwrap();
                let _ = state.sessions.reindex_windows(&name).unwrap();
                Some("#*")
            }
            "ambiguous-reindex" => {
                let _ = state.sessions.reindex_windows(&name).unwrap();
                None
            }
            "slot-replacement" => {
                let _ = state.sessions.remove_window(&target).unwrap();
                let _ = state
                    .sessions
                    .insert_window_with_initial_pane(&name, 7, TARGET_SIZE)
                    .unwrap();
                // Removal selects zero and clears its alerts; install a fresh bell on that link.
                state
                    .sessions
                    .add_winlink_alert_flags(&zero, rmux_core::WINLINK_BELL)
                    .unwrap();
                // W still has exactly one occurrence, at zero: never render the replacement.
                Some("!*")
            }
            "ambiguous-replacement" => {
                let _ = state
                    .sessions
                    .install_window_alias(
                        &WindowTarget::with_window(name.clone(), 3),
                        wid,
                        false,
                        false,
                    )
                    .unwrap();
                let _ = state.sessions.remove_window(&target).unwrap();
                let _ = state
                    .sessions
                    .insert_window_with_initial_pane(&name, 7, TARGET_SIZE)
                    .unwrap();
                None
            }
            "window-deletion" => {
                let _ = state
                    .sessions
                    .insert_window_with_initial_pane(&name, 9, TARGET_SIZE)
                    .unwrap();
                let _ = state.sessions.remove_window(&target).unwrap();
                let _ = state.sessions.remove_window(&zero).unwrap();
                None
            }
            "session-replacement" => {
                let _ = state.sessions.remove_session(&name).unwrap();
                state
                    .sessions
                    .create_session(name.clone(), TARGET_SIZE)
                    .unwrap();
                None
            }
            _ => unreachable!(),
        };
        let event = LifecycleEvent::WindowLayoutChanged { target };
        let mut clients = vec![ControlClientSnapshot {
            pid: 73,
            session_name: Some(owner),
        }];
        if case == "rename-reuse" {
            clients.push(ControlClientSnapshot {
                pid: 74,
                session_name: Some(name),
            });
        }
        let identity = (case != "ordinary").then_some((sid, wid));
        let notifications = collect_control_notifications(&state, &event, None, identity, &clients);
        assert_eq!(
            notifications.len(),
            usize::from(expected.is_some()),
            "{case}: {notifications:?}"
        );
        if let Some(flags) = expected {
            assert_eq!(notifications[0].pid, 73, "{case}");
            let line = &notifications[0].line;
            assert!(
                line.starts_with(&format!("%layout-change {wid} ")),
                "{case}: {line}"
            );
            assert_eq!(
                line.split_whitespace().last(),
                Some(flags),
                "{case}: {line}"
            );
        }
    }
}
