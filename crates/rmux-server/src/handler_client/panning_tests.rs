use super::*;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{NewSessionExtRequest, Request, SessionName, TerminalSize};
use tokio::sync::mpsc;

#[cfg(unix)]
fn quiet_pan_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_pan_command() -> Vec<String> {
    let cmd = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()),
    )
    .join("System32")
    .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn command(handler: &RequestHandler, text: &str) {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new().parse(text).unwrap(),
        )
        .await
        .unwrap();
}

fn request(pid: u32) -> RefreshClientRequest {
    RefreshClientRequest {
        target_client: Some(pid.to_string()),
        adjustment: Some(10),
        clear_pan: false,
        pan_left: false,
        pan_right: true,
        pan_up: false,
        pan_down: false,
        status_only: false,
        clipboard_query: false,
        flags: None,
        flags_alias: None,
        subscriptions: Vec::new(),
        subscriptions_format: Vec::new(),
        control_size: None,
        colour_report: None,
    }
}

async fn fixture(
    pid: u32,
) -> (
    RequestHandler,
    mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let handler = RequestHandler::new();
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(SessionName::new("pan").unwrap()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize {
                cols: 140,
                rows: 50,
            }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_pan_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(
        matches!(response, Response::NewSession(_)),
        "quiet panning fixture should create its session, got {response:?}"
    );
    command(
        &handler,
        "set-option -t pan status off ; set-option -w -t pan window-size manual",
    )
    .await;
    let (tx, rx) = mpsc::unbounded_channel();
    handler
        .register_attach(pid, SessionName::new("pan").unwrap(), tx)
        .await;
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&pid)
        .unwrap()
        .set_declared_client_size(TerminalSize { cols: 80, rows: 24 });
    let cursor = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&SessionName::new("pan").unwrap())
            .unwrap();
        let pane_id = session.window().active_pane().unwrap().id();
        state
            .pane_screen(session.name(), pane_id)
            .map(|screen| screen.cursor_position())
    };
    assert_eq!(
        cursor,
        Some((0, 0)),
        "panning fixture must start from a stable cursor origin"
    );
    (handler, rx)
}

#[tokio::test]
async fn direct_pan_clamps_persistently_and_does_not_mutate_canonical_size() {
    let pid = 92601;
    let (handler, _rx) = fixture(pid).await;
    let mut r = request(pid);
    r.adjustment = Some(i32::MAX as u32);
    assert!(matches!(
        handler.handle(Request::RefreshClient(Box::new(r))).await,
        Response::RefreshClient(_)
    ));
    assert_eq!(
        handler.active_attach.lock().await.by_pid[&pid].viewport.x,
        60
    );
    command(&handler, "resize-window -t pan -x 90 -y 30").await;
    handler.update_client_viewports().await;
    assert_eq!(
        handler.active_attach.lock().await.by_pid[&pid].viewport.x,
        10
    );
    command(&handler, "resize-window -t pan -x 140 -y 50").await;
    handler.update_client_viewports().await;
    assert_eq!(
        handler.active_attach.lock().await.by_pid[&pid].viewport.x,
        10
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&SessionName::new("pan").unwrap())
            .unwrap()
            .window()
            .size(),
        TerminalSize {
            cols: 140,
            rows: 50
        }
    );
}

#[tokio::test]
async fn stale_pan_selection_cannot_mutate_replacement_generation() {
    let pid = 92602;
    let (handler, _rx) = fixture(pid).await;
    let pause = super::super::install_managed_client_resolution_pause(pid);
    let worker = handler.clone();
    let task = tokio::spawn(async move {
        worker
            .handle(Request::RefreshClient(Box::new(request(pid))))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), pause.reached.notified())
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    handler
        .register_attach(pid, SessionName::new("pan").unwrap(), tx)
        .await;
    while rx.try_recv().is_ok() {}
    pause.release.notify_one();
    assert!(matches!(task.await.unwrap(), Response::Error(_)));
    let attaches = handler.active_attach.lock().await;
    assert_eq!(attaches.by_pid[&pid].pan, Default::default());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn invalid_direct_pan_has_no_partial_effect_and_reset_precedes_direction() {
    let pid = 92603;
    let (handler, _rx) = fixture(pid).await;
    assert!(matches!(
        handler
            .handle(Request::RefreshClient(Box::new(request(pid))))
            .await,
        Response::RefreshClient(_)
    ));
    for adjustment in [0, i32::MAX as u32 + 1, u32::MAX] {
        let mut r = request(pid);
        r.adjustment = Some(adjustment);
        r.clear_pan = true;
        assert!(matches!(
            handler.handle(Request::RefreshClient(Box::new(r))).await,
            Response::Error(_)
        ));
        assert_eq!(
            handler.active_attach.lock().await.by_pid[&pid].viewport.x,
            10
        );
    }
    let mut r = request(pid);
    r.clear_pan = true;
    r.flags = Some("invalid-flag".to_owned());
    r.status_only = true;
    assert!(matches!(
        handler.handle(Request::RefreshClient(Box::new(r))).await,
        Response::RefreshClient(_)
    ));
    assert_eq!(
        handler.active_attach.lock().await.by_pid[&pid].viewport.x,
        0
    );
}

#[tokio::test]
async fn redraw_prepared_before_pan_cannot_restore_old_view() {
    let pid = 92604;
    let (handler, mut rx) = fixture(pid).await;
    let pause = super::super::super::attach_support::install_generic_render_delivery_pause(pid);
    let worker = handler.clone();
    let session = SessionName::new("pan").unwrap();
    let task = tokio::spawn(async move { worker.refresh_attached_session(&session).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), pause.reached.notified())
        .await
        .unwrap();
    assert!(matches!(
        handler
            .handle(Request::RefreshClient(Box::new(request(pid))))
            .await,
        Response::RefreshClient(_)
    ));
    while rx.try_recv().is_ok() {}
    pause.release.notify_one();
    task.await.unwrap();
    assert!(
        rx.try_recv().is_err(),
        "stale redraw must not be delivered after pan"
    );
    assert_eq!(
        handler.active_attach.lock().await.by_pid[&pid].viewport.x,
        10
    );
}
