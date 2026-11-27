use super::*;

#[tokio::test]
async fn l05_1_control_response_teardown_rehome_order() {
    for (index, policy, expected) in [
        (0, "on", None),
        (1, "off", Some("z")),
        (2, "no-detached", Some("a")),
        (3, "previous", Some("a")),
        (4, "next", Some("z")),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("a");
        let middle = session_name("m");
        let zulu = session_name("z");
        for name in [&alpha, &zulu, &middle] {
            new_session(&handler, name).await;
        }
        set_detach_on_destroy(&handler, &middle, policy).await;
        let pid = 978_300 + index * 2;
        let mut external = register_control_session(&handler, pid, middle.clone()).await;
        let _ = drain_control_events(&mut external);
        let _occupied = if policy == "no-detached" {
            Some(register_control_session(&handler, pid + 1, zulu.clone()).await)
        } else {
            None
        };
        let old_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&middle)
            .unwrap()
            .id();
        let mut internal = handler.subscribe_lifecycle_events();
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            dispatch_as(
                &handler,
                pid,
                Request::KillSession(KillSessionRequest {
                    target: middle.clone(),
                    kill_all_except_target: false,
                    clear_alerts: false,
                    kill_group: false,
                }),
            ),
        )
        .await
        .expect("teardown/rehome completes before response");
        assert!(
            matches!(response, Response::KillSession(_)),
            "{policy}: {response:?}"
        );
        assert!(!handler
            .state
            .lock()
            .await
            .sessions
            .contains_session(&middle));
        let events = std::iter::from_fn(|| internal.try_recv().ok()).collect::<Vec<_>>();
        let closed = events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.event, LifecycleEvent::SessionClosed { .. }))
            .collect::<Vec<_>>();
        assert_eq!(closed.len(), 1, "{policy}: {events:?}");
        assert_eq!(closed[0].1.event.session_id(), Some(old_id.as_u32()));
        let control = drain_control_events(&mut external);
        if let Some(expected) = expected {
            let destination = session_name(expected);
            let changed = events
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.event, LifecycleEvent::ClientSessionChanged { .. }))
                .collect::<Vec<_>>();
            assert_eq!(changed.len(), 1, "{policy}: {events:?}");
            assert!(
                closed[0].0 < changed[0].0,
                "committed teardown precedes rehome: {events:?}"
            );
            let destination_id = handler
                .state
                .lock()
                .await
                .sessions
                .session(&destination)
                .unwrap()
                .id();
            assert!(
                changed[0]
                    .1
                    .formats
                    .contains(&("hook_session".to_owned(), destination_id.to_string())),
                "{events:?}"
            );
            assert_control_switched_to(&control, &destination);
            assert_eq!(
                control
                    .iter()
                    .filter(|e| matches!(
                        e,
                        ControlServerEvent::SessionChanged(_)
                            | ControlServerEvent::SessionChangedAt { .. }
                    ))
                    .count(),
                1,
                "{control:?}"
            );
        } else {
            assert_eq!(
                control
                    .iter()
                    .filter(|e| matches!(e, ControlServerEvent::Exit(_)))
                    .count(),
                1,
                "{control:?}"
            );
            assert!(!events
                .iter()
                .any(|e| matches!(e.event, LifecycleEvent::ClientSessionChanged { .. })));
        }
        // Reusing the public name creates a distinct lifetime.
        new_session(&handler, &middle).await;
        let replacement_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&middle)
            .unwrap()
            .id();
        assert_ne!(old_id, replacement_id);
    }
}
