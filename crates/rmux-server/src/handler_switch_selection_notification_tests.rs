#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rmux_proto::request::SwitchClientExt3Request;
use rmux_proto::{
    ControlMode, NewSessionRequest, NewWindowRequest, PaneTarget, Request, Response, SessionName,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, TerminalSize,
};
use tokio::sync::mpsc;

use super::RequestHandler;
use crate::client_names::control_client_name;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProtocolSelectionModel {
    client_sessions: HashMap<String, String>,
    session_windows: HashMap<String, String>,
    window_panes: HashMap<String, String>,
    self_client_name: String,
}

impl ProtocolSelectionModel {
    async fn capture(handler: &RequestHandler, self_client_name: String) -> Self {
        let (session_windows, window_panes) = {
            let state = handler.state.lock().await;
            let mut session_windows = HashMap::new();
            let mut window_panes = HashMap::new();
            for (_session_name, session) in state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
            {
                session_windows.insert(session.id().to_string(), session.window().id().to_string());
                for window in session.windows().map(|(_, window)| window) {
                    let pane = window.active_pane().expect("test windows have panes");
                    window_panes.insert(window.id().to_string(), pane.id().to_string());
                }
            }
            (session_windows, window_panes)
        };
        let mut client_sessions = {
            let active_attach = handler.active_attach.lock().await;
            active_attach
                .by_pid
                .values()
                .map(|active| (active.client_name.clone(), active.session_id.to_string()))
                .collect::<HashMap<_, _>>()
        };
        {
            let active_control = handler.active_control.lock().await;
            client_sessions.extend(active_control.by_pid.iter().filter_map(|(pid, active)| {
                active
                    .session_id
                    .map(|session_id| (control_client_name(*pid), session_id.to_string()))
            }));
        }
        Self {
            client_sessions,
            session_windows,
            window_panes,
            self_client_name,
        }
    }

    fn apply(&mut self, notifications: &[String]) {
        for line in notifications {
            let mut fields = line.split_whitespace();
            match fields.next() {
                Some("%session-changed") => {
                    let session_id = fields.next().expect("session-changed session id");
                    self.client_sessions
                        .insert(self.self_client_name.clone(), session_id.to_owned());
                }
                Some("%client-session-changed") => {
                    let client_name = fields.next().expect("client-session-changed client");
                    let session_id = fields.next().expect("client-session-changed session id");
                    self.client_sessions
                        .insert(client_name.to_owned(), session_id.to_owned());
                }
                Some("%session-window-changed") => {
                    let session_id = fields.next().expect("session-window-changed session id");
                    let window_id = fields.next().expect("session-window-changed window id");
                    self.session_windows
                        .insert(session_id.to_owned(), window_id.to_owned());
                }
                Some("%window-pane-changed") => {
                    let window_id = fields.next().expect("window-pane-changed window id");
                    let pane_id = fields.next().expect("window-pane-changed pane id");
                    self.window_panes
                        .insert(window_id.to_owned(), pane_id.to_owned());
                }
                _ => {}
            }
        }
    }

    async fn assert_current(&self, handler: &RequestHandler, context: &str) {
        let actual = Self::capture(handler, self.self_client_name.clone()).await;
        assert_eq!(
            self.client_sessions, actual.client_sessions,
            "{context}: protocol consumer client sessions"
        );
        assert_eq!(
            self.session_windows, actual.session_windows,
            "{context}: protocol consumer active windows"
        );
        assert_eq!(
            self.window_panes, actual.window_panes,
            "{context}: protocol consumer active panes"
        );
    }
}

struct SwitchFixture {
    handler: RequestHandler,
    source: SessionName,
    target: SessionName,
}

impl SwitchFixture {
    async fn new(label: &str) -> Self {
        let handler = RequestHandler::new();
        let source = session_name(&format!("{label}-source"));
        let target = session_name(&format!("{label}-target"));
        create_session(&handler, &source).await;
        create_session(&handler, &target).await;
        split_window(&handler, &target, 0).await;
        let second_window = new_detached_window(&handler, &target).await;
        assert_eq!(second_window, 1);
        split_window(&handler, &target, second_window).await;
        handler.wait_for_initial_panes_for_test().await;

        {
            let mut state = handler.state.lock().await;
            let mut target_session = state
                .sessions
                .session_setup(&target)
                .expect("target session exists");
            target_session
                .select_pane_in_window(0, 0)
                .expect("first pane selected");
            target_session
                .select_pane_in_window(1, 0)
                .expect("first pane selected in inactive window");
            target_session
                .select_window(0)
                .expect("first window selected");
        }

        Self {
            handler,
            source,
            target,
        }
    }
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, session_name: &SessionName) {
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
}

async fn new_detached_window(handler: &RequestHandler, session_name: &SessionName) -> u32 {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("new-window failed: {response:?}");
    };
    response.target.window_index()
}

async fn split_window(handler: &RequestHandler, session_name: &SessionName, window_index: u32) {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(
                session_name.clone(),
                window_index,
                0,
            )),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
}

async fn register_control(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: &SessionName,
) -> (u64, mpsc::Receiver<ControlServerEvent>) {
    let (event_tx, event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(session_name.clone()))
        .await
        .expect("control session set");
    (control_id, event_rx)
}

fn drain_notifications(rx: &mut mpsc::Receiver<ControlServerEvent>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ControlServerEvent::Notification(line) => lines.push(line),
            ControlServerEvent::SessionChanged(_)
            | ControlServerEvent::SessionChangedAt { .. }
            | ControlServerEvent::FormatSubscriptions(_)
            | ControlServerEvent::PaneOutputActions(_)
            | ControlServerEvent::Refresh => {}
            ControlServerEvent::Exit(reason) => panic!("unexpected control exit: {reason:?}"),
        }
    }
    lines
}

fn transition_order(notifications: &[String]) -> Vec<&str> {
    notifications
        .iter()
        .filter_map(|line| {
            let event = line.split_whitespace().next()?;
            matches!(
                event,
                "%window-pane-changed"
                    | "%session-window-changed"
                    | "%client-session-changed"
                    | "%session-changed"
            )
            .then_some(event)
        })
        .collect()
}

fn switch_request(target: String) -> SwitchClientExt3Request {
    SwitchClientExt3Request {
        target_client: None,
        target: Some(target),
        key_table: None,
        last_session: false,
        next_session: false,
        previous_session: false,
        toggle_read_only: false,
        sort_order: None,
        skip_environment_update: true,
        zoom: false,
    }
}

#[tokio::test]
async fn pty_switch_selection_notifications_keep_protocol_model_current() {
    let cases = [
        (
            "window",
            ":1",
            vec!["%session-window-changed", "%client-session-changed"],
        ),
        (
            "pane",
            ":0.1",
            vec!["%window-pane-changed", "%client-session-changed"],
        ),
        (
            "window-pane",
            ":1.1",
            vec![
                "%window-pane-changed",
                "%session-window-changed",
                "%client-session-changed",
            ],
        ),
        ("active", ":0.0", vec!["%client-session-changed"]),
        ("session", "", vec!["%client-session-changed"]),
    ];

    for (offset, (label, suffix, expected_order)) in cases.into_iter().enumerate() {
        let fixture = SwitchFixture::new(&format!("pty-switch-{label}")).await;
        let attach_pid = 71_000 + u32::try_from(offset).expect("small test offset");
        let observer_pid = attach_pid + 1_000;
        let (_observer_id, mut observer_rx) =
            register_control(&fixture.handler, observer_pid, &fixture.target).await;
        let (attach_tx, mut attach_rx) = mpsc::unbounded_channel();
        fixture
            .handler
            .register_attach(attach_pid, fixture.source.clone(), attach_tx)
            .await;
        let _ = drain_notifications(&mut observer_rx);
        while attach_rx.try_recv().is_ok() {}

        let mut model =
            ProtocolSelectionModel::capture(&fixture.handler, control_client_name(observer_pid))
                .await;
        let response = fixture
            .handler
            .handle_switch_client_ext3(
                attach_pid,
                switch_request(format!("{}{suffix}", fixture.target)),
            )
            .await;
        assert!(
            matches!(response, Response::SwitchClient(_)),
            "{label}: {response:?}"
        );

        let notifications = drain_notifications(&mut observer_rx);
        assert_eq!(
            transition_order(&notifications),
            expected_order,
            "{label}: tmux 3.7b transition order"
        );
        model.apply(&notifications);
        model.assert_current(&fixture.handler, label).await;

        if label == "window-pane" {
            let repeated = fixture
                .handler
                .handle_switch_client_ext3(
                    attach_pid,
                    switch_request(format!("{}:1.1", fixture.target)),
                )
                .await;
            assert!(
                matches!(repeated, Response::SwitchClient(_)),
                "{repeated:?}"
            );
            let repeated_notifications = drain_notifications(&mut observer_rx);
            assert_eq!(
                transition_order(&repeated_notifications),
                vec!["%client-session-changed"],
                "a repeated switch emits no selection transition"
            );
            model.apply(&repeated_notifications);
            model
                .assert_current(&fixture.handler, "window-pane repetition")
                .await;
        }
    }
}

#[tokio::test]
async fn control_self_switch_selection_notifications_keep_protocol_model_current() {
    let fixture = SwitchFixture::new("control-self-switch").await;
    let requester_pid = 72_000;
    let (control_id, mut event_rx) =
        register_control(&fixture.handler, requester_pid, &fixture.source).await;
    let _ = drain_notifications(&mut event_rx);
    let mut model =
        ProtocolSelectionModel::capture(&fixture.handler, control_client_name(requester_pid)).await;

    let command = format!("switch-client -t {}:1.1", fixture.target);
    let commands = fixture
        .handler
        .parse_control_commands(&command)
        .await
        .expect("control switch parses");
    let result = fixture
        .handler
        .execute_control_commands_identity(requester_pid, control_id, commands)
        .await;
    assert!(result.error.is_none(), "{:?}", result.error);

    let notifications = drain_notifications(&mut event_rx);
    assert_eq!(
        transition_order(&notifications),
        vec![
            "%window-pane-changed",
            "%session-window-changed",
            "%session-changed",
        ],
        "tmux 3.7b emits pane, window, then the switching control client"
    );
    model.apply(&notifications);
    model
        .assert_current(&fixture.handler, "control self switch")
        .await;

    let repeated = fixture
        .handler
        .parse_control_commands(&command)
        .await
        .expect("repeated control switch parses");
    let result = fixture
        .handler
        .execute_control_commands_identity(requester_pid, control_id, repeated)
        .await;
    assert!(result.error.is_none(), "{:?}", result.error);
    let repeated_notifications = drain_notifications(&mut event_rx);
    assert_eq!(
        transition_order(&repeated_notifications),
        vec!["%session-changed"],
        "a repeated control switch emits no selection transition"
    );
    model.apply(&repeated_notifications);
    model
        .assert_current(&fixture.handler, "control self repetition")
        .await;
}
