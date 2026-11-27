use super::*;

#[tokio::test]
async fn session_destruction_notifies_removed_control_before_exit() {
    for operation in ["session", "window", "pane"] {
        let handler = RequestHandler::new();
        let name = session_name("destruction-target");
        new_session(&handler, &name).await;
        let mut rx = register_control_session(&handler, 43_080, name.clone()).await;
        let _ = drain_control_events(&mut rx);
        let request = match operation {
            "session" => Request::KillSession(KillSessionRequest {
                target: name,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }),
            "window" => Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(name, 0),
                kill_all_others: false,
            }),
            _ => Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(name, 0, 0),
                kill_all_except: false,
            }),
        };
        let response = handler.handle(request).await;
        assert!(!matches!(response, Response::Error(_)), "{response:?}");
        let events = drain_control_events(&mut rx);
        let close = events.iter().position(|event| {
            matches!(event,
            ControlServerEvent::Notification(line) if line == "%sessions-changed")
        });
        let exit = events
            .iter()
            .position(|event| matches!(event, ControlServerEvent::Exit(_)));
        assert!(close.is_some(), "{operation}: {events:?}");
        assert!(
            exit.is_some_and(|exit| close.unwrap() < exit),
            "{operation}: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event,
            ControlServerEvent::Notification(line) if line == "%sessions-changed"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn session_destruction_notifies_survivor_and_rehomed_control_once() {
    for rehome in [false, true] {
        let handler = RequestHandler::new();
        let removed = session_name("destruction-removed");
        let survivor = session_name("destruction-survivor");
        new_session(&handler, &removed).await;
        new_session(&handler, &survivor).await;
        if rehome {
            set_detach_on_destroy(&handler, &removed, "off").await;
        }
        let mut removed_rx = register_control_session(&handler, 43_081, removed.clone()).await;
        let mut survivor_rx = register_control_session(&handler, 43_082, survivor.clone()).await;
        let _ = drain_control_events(&mut removed_rx);
        let _ = drain_control_events(&mut survivor_rx);
        let response = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: removed,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(response, Response::KillSession(_)), "{response:?}");
        for (is_removed, rx) in [(true, &mut removed_rx), (false, &mut survivor_rx)] {
            let events = drain_control_events(rx);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event,
                ControlServerEvent::Notification(line) if line == "%sessions-changed"))
                    .count(),
                1,
                "rehome={rehome}, removed={is_removed}: {events:?}"
            );
            assert_eq!(
                events
                    .iter()
                    .any(|event| matches!(event, ControlServerEvent::Exit(_))),
                is_removed && !rehome,
                "{events:?}"
            );
        }
    }
}
