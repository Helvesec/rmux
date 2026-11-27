use super::*;
use crate::handler::test_support::SERVER_SIGNAL_LIVENESS;

async fn respawn(handler: &RequestHandler, target: &WindowTarget, kill: bool) -> Response {
    handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: target.clone(),
            kill,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
        })))
        .await
}

async fn pid(handler: &RequestHandler, target: &PaneTarget) -> u32 {
    handler
        .state
        .lock()
        .await
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .expect("pane has a process identity")
}

async fn wait_dead(handler: &RequestHandler, target: &PaneTarget) {
    let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
    loop {
        let response = handler
            .handle(Request::DisplayMessage(DisplayMessageRequest {
                target: Some(Target::Pane(target.clone())),
                print: true,
                message: Some("#{pane_dead}".to_owned()),
                empty_target_context: false,
            }))
            .await;
        let Response::DisplayMessage(response) = response else {
            panic!("expected pane_dead, got {response:?}");
        };
        if response
            .command_output()
            .expect("printed pane_dead")
            .stdout()
            == b"1\n"
        {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "pane did not exit");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn respawn_window_retained_dead_panes_need_no_kill_but_live_panes_do() {
    // tmux 3.7b: all panes must have exited for respawn-window without -k.
    // A retained terminal and its old PID must not count as a live workload.
    for pane_count in [1, 2] {
        let handler = RequestHandler::new();
        let name = session_name("respawn-retained-dead");
        let window = WindowTarget::with_window(name.clone(), 0);
        let first = PaneTarget::with_window(name.clone(), 0, 0);
        create_session(&handler, name.as_str()).await;
        if pane_count == 2 {
            let split = handler
                .handle(Request::SplitWindow(SplitWindowRequest {
                    target: SplitWindowTarget::Session(name.clone()),
                    direction: SplitDirection::Vertical,
                    before: false,
                    environment: None,
                }))
                .await;
            assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
        }
        handler.wait_for_initial_panes_for_test().await;
        let option = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(window.clone()),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(option, Response::SetOption(_)), "{option:?}");
        let first_id = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&name)
                .unwrap()
                .pane_id_in_window(0, 0)
                .unwrap()
        };

        for index in 0..pane_count {
            let target = PaneTarget::with_window(name.clone(), 0, index);
            let exited = handler
                .handle(Request::RespawnPane(Box::new(
                    rmux_proto::RespawnPaneRequest {
                        target: target.clone(),
                        kill: true,
                        start_directory: None,
                        environment: None,
                        command: Some(vec!["exit 0".to_owned()]),
                        process_command: None,
                    },
                )))
                .await;
            assert!(matches!(exited, Response::RespawnPane(_)), "{exited:?}");
            wait_dead(&handler, &target).await;
            if index + 1 < pane_count {
                let live = PaneTarget::with_window(name.clone(), 0, index + 1);
                let before = pid(&handler, &live).await;
                let denied = respawn(&handler, &window, false).await;
                assert!(matches!(denied, Response::Error(_)), "{denied:?}");
                assert_eq!(pid(&handler, &live).await, before);
            }
        }

        let dead_pid = pid(&handler, &first).await;
        {
            let state = handler.state.lock().await;
            state
                .ensure_panes_exist(&name, &[first_id])
                .expect("dead terminal is retained");
        }
        let restarted = respawn(&handler, &window, false).await;
        assert!(
            matches!(restarted, Response::RespawnWindow(_)),
            "all {pane_count} panes are dead; respawn without -k must succeed: {restarted:?}"
        );
        handler.wait_for_initial_panes_for_test().await;
        let live_pid = pid(&handler, &first).await;
        assert_ne!(live_pid, dead_pid);
        {
            let state = handler.state.lock().await;
            let window = state.sessions.session(&name).unwrap().window_at(0).unwrap();
            assert_eq!(window.panes().len(), 1);
            assert_eq!(window.pane(0).unwrap().id(), first_id);
        }
        let denied = respawn(&handler, &window, false).await;
        assert!(matches!(denied, Response::Error(_)), "{denied:?}");
        assert_eq!(pid(&handler, &first).await, live_pid);
        let forced = respawn(&handler, &window, true).await;
        assert!(matches!(forced, Response::RespawnWindow(_)), "{forced:?}");
        handler.wait_for_initial_panes_for_test().await;
        assert_ne!(pid(&handler, &first).await, live_pid);
    }
}
