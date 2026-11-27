//! N1 must carry the resolved target identity through the existing switch lock.
use super::*;
use rmux_proto::request::AttachSessionExt2Request;
use rmux_proto::{
    NewSessionRequest, Request, ScopeSelector, SetOptionMode, SetOptionRequest, TerminalSize,
};
use tokio::sync::mpsc;

async fn create(handler: &RequestHandler, name: &SessionName) {
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
                session_name: name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    handler.wait_for_initial_pane_tasks_for_test().await;
}

async fn commands(handler: &RequestHandler, text: &str) {
    let parsed = handler.parse_control_commands(text).await.unwrap();
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap();
}

async fn replaced_target(kind: &str) {
    let handler = RequestHandler::new();
    let alpha = SessionName::new(format!("n1-stable-{kind}-alpha")).unwrap();
    let beta = SessionName::new(format!("n1-stable-{kind}-beta")).unwrap();
    create(&handler, &alpha).await;
    create(&handler, &beta).await;
    commands(
        &handler,
        &format!("new-window -d -t {beta}:1 ; split-window -dI -t {beta}:1.0"),
    )
    .await;
    let pid = 96_201;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let attach_id = handler.register_attach(pid, alpha.clone(), tx).await;
    let pause = install_switch_target_identity_pause(beta.clone());
    let request = AttachSessionExt2Request {
        target: Some(beta.clone()),
        target_spec: Some(format!("{beta}:1.0")),
        detach_other_clients: false,
        kill_other_clients: false,
        read_only: false,
        skip_environment_update: true,
        flags: None,
        working_directory: None,
        client_terminal: Default::default(),
        client_size: None,
    };
    let worker = handler.clone();
    let pending = tokio::spawn(async move {
        worker
            .dispatch(pid, Request::AttachSessionExt2(Box::new(request)))
            .await
            .response
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), pause.reached.notified())
        .await
        .unwrap();
    let active_before_commit = handler
        .state
        .lock()
        .await
        .sessions
        .session(&beta)
        .unwrap()
        .active_window_index();
    match kind {
        "session" => {
            commands(&handler, &format!("kill-session -t {beta}")).await;
            create(&handler, &beta).await;
        }
        "window" => {
            commands(
                &handler,
                &format!("kill-window -t {beta}:1 ; new-window -d -t {beta}:1"),
            )
            .await
        }
        // Removing pane 0 rekeys the surviving Empty pane to index 0.
        "pane" => commands(&handler, &format!("kill-pane -t {beta}:1.0")).await,
        _ => unreachable!(),
    }
    pause.release.notify_one();
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        active_before_commit, 0,
        "selection must wait for the identity-checked commit"
    );
    assert!(
        matches!(response, Response::Error(_)),
        "reused {kind} target: {response:?}"
    );
    let clients = handler.active_attach.lock().await;
    let active = clients.by_pid.get(&pid).unwrap();
    assert_eq!(active.id, attach_id);
    assert_eq!(active.session_name, alpha);
    assert!(rx.try_recv().is_err(), "no stale target frame delivered");
}

#[tokio::test]
async fn attach_target_identity_rejects_recreated_session() {
    replaced_target("session").await;
}
#[tokio::test]
async fn attach_target_identity_rejects_reused_window_index() {
    replaced_target("window").await;
}
#[tokio::test]
async fn attach_target_identity_rejects_reused_pane_index() {
    replaced_target("pane").await;
}

#[tokio::test]
async fn control_attach_ignore_size_failed_switch_preserves_flags_geometry_and_publication() {
    use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
    use rmux_proto::ControlMode;
    use std::sync::{atomic::AtomicBool, Arc};
    use std::time::Duration;

    async fn notifications(events: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(250), events.recv()).await
        {
            if let ControlServerEvent::Notification(line) = event {
                lines.push(line);
            }
        }
        lines
    }

    let handler = RequestHandler::new();
    let source = SessionName::new("control-attach-rollback-source").unwrap();
    let target = SessionName::new("control-attach-rollback-target").unwrap();
    create(&handler, &source).await;
    create(&handler, &target).await;
    commands(
        &handler,
        &format!("set-window-option -t {source}:0 window-size largest"),
    )
    .await;
    let pid = 96_211;
    let mut streams = Vec::new();
    let mut control_id = None;
    for (client_pid, size) in [(pid, "100,40"), (pid + 1, "80,24")] {
        let (tx, rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let id = handler
            .register_control_with_closing(
                client_pid,
                ControlModeUpgrade {
                    initial_command_count: 0,
                    mode: ControlMode::Plain,
                    terminal_context: Default::default(),
                },
                tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        control_id.get_or_insert(id);
        handler
            .set_control_session(client_pid, Some(source.clone()))
            .await
            .unwrap();
        let parsed = handler
            .parse_control_commands(&format!("refresh-client -C {size}"))
            .await
            .unwrap();
        handler
            .execute_parsed_commands_for_test(client_pid, parsed)
            .await
            .unwrap();
        streams.push(rx);
    }
    let expected_size = TerminalSize {
        cols: 100,
        rows: 40,
    };
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&source)
            .unwrap()
            .window()
            .size(),
        expected_size
    );
    for stream in &mut streams {
        notifications(stream).await;
    }
    let pause = install_switch_target_identity_pause(target.clone());
    let worker = handler.clone();
    let attach_target = target.clone();
    let pending = tokio::spawn(async move {
        let parsed = worker
            .parse_control_commands(&format!("attach -f ignore-size -t {attach_target}"))
            .await
            .unwrap();
        worker.execute_parsed_commands_for_test(pid, parsed).await
    });
    tokio::time::timeout(Duration::from_secs(10), pause.reached.notified())
        .await
        .unwrap();
    commands(&handler, &format!("kill-session -t {target}")).await;
    // Destruction has its own valid notifications; only the failed attach must
    // leave the surviving clients' flags, geometry and publication unchanged.
    for stream in &mut streams {
        notifications(stream).await;
    }
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err(), "destination vanished: {result:?}");
    {
        let clients = handler.active_control.lock().await;
        let active = clients.by_pid.get(&pid).unwrap();
        assert_eq!(Some(active.id), control_id);
        assert_eq!(active.session_name.as_ref(), Some(&source));
        assert!(
            !active.flags.ignore_size,
            "failed attach must not change size eligibility"
        );
        assert!(!crate::handler::format_control_client_flags(active).contains("ignore-size"));
        assert_eq!((active.client_width, active.client_height), (100, 40));
    }
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&source)
            .unwrap()
            .window()
            .size(),
        expected_size
    );
    for stream in &mut streams {
        let lines = notifications(stream).await;
        assert!(
            !lines.iter().any(|line| line.starts_with("%layout-change")
                || line.starts_with("%client-session-changed")),
            "failed attach published a transition: {lines:?}"
        );
    }
}
