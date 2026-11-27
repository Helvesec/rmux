//! The default target of `send-keys -c <client>` without `-t`, against tmux
//! 3.7b (measured 2026-11-19: sessions t then s, the named client on t, a
//! control client attached to s last). tmux resolves `-t` from the queue
//! item's own client (`cmd_find_from_client()`), never from the client `-c`
//! names: a requester without a client types into the most recently active
//! session, s, whether the named client is a terminal or a control client and
//! whether the command comes as a request or off a parsed queue; a control
//! client sending it types into its own session.

use super::read_only_tests::run_control_command;
use super::*;
use crate::handler::control_support::ControlClientIdentity;

/// The client `send-keys -c` names, on `alpha`, attached before the control
/// client of `beta`.
#[derive(Clone, Copy, Debug)]
enum NamedClient {
    Terminal,
    Control,
}

/// How a requester without a client of its own sends the keys.
#[derive(Clone, Copy, Debug)]
enum Route {
    Request,
    ParsedQueue,
}

/// The focus mode of alpha's pane when the clients come. A quiet child leaves
/// DEC 1004 off on a Unix PTY, but the ConPTY startup preamble turns it on, so
/// on Windows attaching the terminal client of alpha writes a focus-in to
/// alpha's pane, as tmux does for a focused client. That report is not a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlphaFocusReports {
    Off,
    On,
}

struct TwoSessions {
    handler: RequestHandler,
    alpha: rmux_proto::PaneTarget,
    beta: rmux_proto::PaneTarget,
}

impl TwoSessions {
    /// `alpha` then `beta`, so the most recently active session is neither
    /// the first by name nor the session of a client attached to `alpha`.
    /// Both panes start with focus reports off on every platform; `alpha_focus`
    /// then sets alpha's.
    async fn new(alpha_focus: AlphaFocusReports) -> Self {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        let beta = session_name("beta");
        create_session(&handler, alpha.clone(), INITIAL_SIZE).await;
        create_session(&handler, beta.clone(), INITIAL_SIZE).await;
        let alpha = rmux_proto::PaneTarget::new(alpha, 0);
        let beta = rmux_proto::PaneTarget::new(beta, 0);
        handler.disable_pane_focus_reports_for_test(&alpha).await;
        handler.disable_pane_focus_reports_for_test(&beta).await;
        {
            let mut state = handler.state.lock().await;
            if alpha_focus == AlphaFocusReports::On {
                state
                    .append_bytes_to_pane_transcript_for_test(
                        alpha.session_name(),
                        alpha.window_index(),
                        alpha.pane_index(),
                        b"\x1b[?1004h",
                    )
                    .expect("alpha's pane enables focus reports");
            }
            state.start_pane_input_capture_for_test(&alpha);
            state.start_pane_input_capture_for_test(&beta);
        }
        Self {
            handler,
            alpha,
            beta,
        }
    }

    /// Every byte written to each pane.
    async fn typed(&self) -> (Vec<u8>, Vec<u8>) {
        let state = self.handler.state.lock().await;
        let typed = |target| {
            state
                .pane_input_capture_for_test(target)
                .expect("pane input is captured")
        };
        (typed(&self.alpha), typed(&self.beta))
    }

    /// The bytes written to each pane, without the spans the committed focus
    /// producer attributed to itself.
    async fn typed_keys(&self) -> (Vec<u8>, Vec<u8>) {
        let state = self.handler.state.lock().await;
        let typed = |target| {
            state
                .pane_non_focus_input_capture_for_test(target)
                .expect("pane input is captured")
        };
        (typed(&self.alpha), typed(&self.beta))
    }
}

async fn send_keys_naming_a_client_of_another_session(
    sessions: &TwoSessions,
    named: NamedClient,
    route: Route,
) {
    let handler = &sessions.handler;
    let requester_pid = std::process::id();
    let named_pid = requester_pid.wrapping_add(17);
    // Each registration's receiver lives as long as the test, as a client's
    // connection would.
    let mut _named_attach = None;
    let mut _named_control = None;
    let name = match named {
        NamedClient::Terminal => {
            _named_attach = Some(
                register_attached_client(
                    handler,
                    named_pid,
                    sessions.alpha.session_name(),
                    INITIAL_SIZE,
                )
                .await,
            );
            named_pid.to_string()
        }
        NamedClient::Control => {
            _named_control = Some(
                register_control_client_with_id(handler, named_pid, sessions.alpha.session_name())
                    .await,
            );
            crate::client_names::control_client_name(named_pid)
        }
    };
    let latest_pid = requester_pid.wrapping_add(19);
    let (latest_id, _latest_events) =
        register_control_client_with_id(handler, latest_pid, sessions.beta.session_name()).await;
    // Attaching, as `tmux -C attach -t beta` does, makes beta the most
    // recently active session.
    run_control_command(
        handler,
        ControlClientIdentity::new(latest_pid, latest_id),
        "attach-session -t beta",
    )
    .await
    .expect("the latest control client attaches to beta");

    match route {
        Route::Request => {
            let response = handler
                .dispatch(
                    requester_pid,
                    Request::SendKeysExt2(Box::new(rmux_proto::SendKeysExt2Request {
                        target: None,
                        keys: vec!["G".to_owned()],
                        expand_formats: false,
                        hex: false,
                        literal: true,
                        dispatch_key_table: false,
                        copy_mode_command: false,
                        forward_mouse_event: false,
                        reset_terminal: false,
                        repeat_count: None,
                        target_client: Some(name),
                    })),
                )
                .await
                .response;
            assert!(
                matches!(
                    response,
                    Response::SendKeys(rmux_proto::SendKeysResponse { key_count: 1 })
                ),
                "{named:?} client named: {response:?}"
            );
        }
        Route::ParsedQueue => {
            let parsed = CommandParser::new()
                .parse(&format!("send-keys -c {name} -l G"))
                .expect("send-keys parses");
            handler
                .execute_parsed_commands_for_test(requester_pid, parsed)
                .await
                .unwrap_or_else(|error| panic!("{named:?} client named: {error}"));
        }
    }
}

#[tokio::test]
async fn send_keys_naming_a_client_takes_its_default_target_from_the_requester() {
    for named in [NamedClient::Terminal, NamedClient::Control] {
        for route in [Route::Request, Route::ParsedQueue] {
            let sessions = TwoSessions::new(AlphaFocusReports::Off).await;
            send_keys_naming_a_client_of_another_session(&sessions, named, route).await;
            assert_eq!(
                sessions.typed().await,
                (Vec::new(), b"G".to_vec()),
                "-c names the {named:?} client of alpha ({route:?}): the keys go to beta, \
                 the most recently active session"
            );
        }
    }
}

/// Windows CI r11: with alpha's pane reporting focus, the focus-in its
/// terminal client brings is no key, and the keys still go to beta.
#[tokio::test]
async fn send_keys_naming_a_client_of_a_focus_reporting_pane_still_types_into_beta() {
    for named in [NamedClient::Terminal, NamedClient::Control] {
        for route in [Route::Request, Route::ParsedQueue] {
            let sessions = TwoSessions::new(AlphaFocusReports::On).await;
            send_keys_naming_a_client_of_another_session(&sessions, named, route).await;
            assert_eq!(
                sessions.typed_keys().await,
                (Vec::new(), b"G".to_vec()),
                "-c names the {named:?} client of alpha ({route:?}), whose pane reports \
                 focus: the keys go to beta, the most recently active session"
            );
            // tmux 3.7b, measured 2026-11-19: attaching a terminal client
            // writes ESC [ I to a pane that asked for focus reports.
            if matches!(named, NamedClient::Terminal) {
                assert_eq!(
                    sessions.typed().await.0,
                    b"\x1b[I".to_vec(),
                    "alpha's pane gets the focus-in of its terminal client ({route:?})"
                );
            }
        }
    }
}

#[tokio::test]
async fn control_client_naming_another_client_types_into_its_own_session() {
    let sessions = TwoSessions::new(AlphaFocusReports::Off).await;
    let handler = &sessions.handler;
    let requester_pid = std::process::id();
    let other_pid = requester_pid.wrapping_add(17);
    let own_pid = requester_pid.wrapping_add(19);
    let (_other_id, _other_events) =
        register_control_client_with_id(handler, other_pid, sessions.beta.session_name()).await;
    let (own_id, _own_events) =
        register_control_client_with_id(handler, own_pid, sessions.alpha.session_name()).await;
    let other = crate::client_names::control_client_name(other_pid);

    run_control_command(
        handler,
        ControlClientIdentity::new(own_pid, own_id),
        &format!("send-keys -c {other} -l G"),
    )
    .await
    .expect("a control client may send keys for another client");
    assert_eq!(sessions.typed().await, (b"G".to_vec(), Vec::new()));
}
