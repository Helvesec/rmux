use super::RequestHandler;
use crate::pane_io::AttachControl;
use rmux_proto::{
    DisplayMessageRequest, NewSessionRequest, NewWindowRequest, OptionName, OptionScopeSelector,
    PaneTarget, Request, Response, ScopeSelector, SelectPaneMarkRequest, SessionName,
    SetOptionMode, SplitDirection, SplitWindowRequest, SplitWindowTarget, Target, TerminalSize,
};
use std::path::Path;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn default_shell_window_name() -> String {
    std::env::var_os("SHELL")
        .and_then(|shell| Path::new(&shell).file_name().map(|name| name.to_owned()))
        .map(|name| name.to_string_lossy().trim_start_matches('-').to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "sh".to_owned())
}

async fn recv_overlay_control(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> AttachControl {
    loop {
        match control_rx.recv().await.expect("overlay control") {
            AttachControl::Switch(_) => {}
            control => return control,
        }
    }
}

#[tokio::test]
async fn display_message_print_expands_shared_formats_without_attached_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::new(alpha, 0))),
            print: true,
            message: Some(
                "#{session_name}:#{session_windows}:#{window_index}:#{pane_index}:#{pane_active}"
                    .to_owned(),
            ),
        }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"alpha:1:0:0:1\n");
}

#[tokio::test]
async fn display_message_last_window_index_is_highest_session_window_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: Some("detached".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::new(alpha, 0))),
            print: true,
            message: Some("#{active_window_index}:#{last_window_index}".to_owned()),
        }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"0:1\n");
}

#[tokio::test]
async fn display_message_print_uses_full_detached_geometry_for_window_and_pane_formats() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::new(alpha, 0))),
            print: true,
            message: Some(
                "#{session_width}x#{session_height}|#{window_width}x#{window_height}|#{window_layout}|#{pane_width}x#{pane_height}"
                    .to_owned(),
            ),
            }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    let rendered = std::str::from_utf8(output.stdout()).expect("utf-8 output");
    let (prefix, suffix) = rendered
        .trim_end()
        .split_once('|')
        .expect("formatted output contains separators");
    assert_eq!(prefix, "x");
    let mut parts = suffix.split('|');
    assert_eq!(parts.next(), Some("80x24"));
    let layout = parts.next().expect("layout part");
    assert_eq!(
        layout.split_once(',').expect("layout checksum").1,
        "80x24,0,0[80x12,0,0,0,80x11,0,13,1]"
    );
    assert_eq!(parts.next(), Some("80x12"));
}

#[tokio::test]
async fn display_message_print_uses_lone_session_context_for_user_options() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set_by_name(
                OptionScopeSelector::SessionGlobal,
                "@my-user-opt",
                Some("hello-world".to_owned()),
                SetOptionMode::Replace,
                false,
                false,
                false,
            )
            .expect("user option set succeeds");
    }

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: None,
            print: true,
            message: Some("opt=#{@my-user-opt}".to_owned()),
        }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"opt=hello-world\n");
}

#[tokio::test]
async fn display_message_print_leaves_lone_session_size_formats_empty_without_explicit_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: None,
            print: true,
            message: Some(
                "#{session_name}|#{session_attached}|#{session_width}|#{session_height}|#{window_width}|#{window_height}|#{pane_width}|#{pane_height}"
                    .to_owned(),
            ),
            }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"alpha|0|||80|24|80|24\n");
}

#[tokio::test]
async fn display_message_print_uses_stored_default_window_name_for_detached_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Session(alpha)),
            print: true,
            message: Some("#{window_name}".to_owned()),
        }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(
        output.stdout(),
        format!("{}\n", default_shell_window_name()).as_bytes()
    );
}

#[tokio::test]
async fn display_message_print_reports_marked_pane_runtime_flags() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                direction: SplitDirection::Horizontal,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectPaneMark(SelectPaneMarkRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 1),
                clear: false,
                title: None,
            }))
            .await,
        Response::SelectPane(_)
    ));

    let format = "#{pane_marked}|#{pane_marked_set}|#{session_marked}|#{window_marked_flag}";
    let pane0 = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0))),
            print: true,
            message: Some(format.to_owned()),
        }))
        .await;
    let pane1 = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 1))),
            print: true,
            message: Some(format.to_owned()),
        }))
        .await;

    let Response::DisplayMessage(pane0) = pane0 else {
        panic!("expected display-message response for pane 0");
    };
    let Response::DisplayMessage(pane1) = pane1 else {
        panic!("expected display-message response for pane 1");
    };
    assert_eq!(
        pane0
            .command_output()
            .expect("display-message -p returns output")
            .stdout(),
        b"0|1|1|1\n"
    );
    assert_eq!(
        pane1
            .command_output()
            .expect("display-message -p returns output")
            .stdout(),
        b"1|1|1|1\n"
    );
}

#[tokio::test]
async fn display_message_print_treats_flag_options_like_tmux_in_conditionals() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))),
            print: true,
            message: Some("#{synchronize-panes}|#{?synchronize-panes,yes,no}".to_owned()),
        }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"0|no\n");
}

#[tokio::test]
async fn display_message_print_expands_runtime_session_window_and_pane_loops() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                direction: SplitDirection::Horizontal,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let window_name = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0))),
            print: true,
            message: Some("#{window_name}".to_owned()),
        }))
        .await;
    let Response::DisplayMessage(window_name) = window_name else {
        panic!("expected display-message response for window name");
    };
    let window_name = String::from_utf8(
        window_name
            .command_output()
            .expect("display-message -p returns output")
            .stdout()
            .to_vec(),
    )
    .expect("window name output is utf-8");
    let window_name = window_name.trim_end().to_owned();

    let loops = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))),
            print: true,
            message: Some("#{S:#W}|#{W:#W}|#{P:#{pane_index}}|#{N:#W}".to_owned()),
        }))
        .await;
    let Response::DisplayMessage(loops) = loops else {
        panic!("expected display-message response for runtime loops");
    };
    assert_eq!(
        loops
            .command_output()
            .expect("display-message -p returns output")
            .stdout(),
        format!("{window_name}|{window_name}|01|1\n").as_bytes()
    );
}

#[tokio::test]
async fn display_message_name_exists_modifier_checks_window_names_not_window_count() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::NewWindow(NewWindowRequest {
                target: alpha.clone(),
                name: Some("w1".to_owned()),
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
            }))
            .await,
        Response::NewWindow(_)
    ));

    let name_exists = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))),
            print: true,
            message: Some("#{N:#W}|#{N/w:w1}|#{N/s:alpha}|#{N/s:missing}".to_owned()),
        }))
        .await;
    let Response::DisplayMessage(name_exists) = name_exists else {
        panic!("expected display-message response for name-exists modifiers");
    };
    assert_eq!(
        name_exists
            .command_output()
            .expect("display-message -p returns output")
            .stdout(),
        b"1|1|1|0\n"
    );
}

#[tokio::test]
async fn bare_display_message_without_target_or_attached_client_is_a_silent_noop() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: None,
            print: false,
            message: Some("unused".to_owned()),
        }))
        .await;

    assert_eq!(
        response,
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );
}

#[tokio::test]
async fn bare_display_message_uses_status_overlay_for_attached_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler.register_attach(42, alpha.clone(), control_tx).await;

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Session(alpha)),
            print: false,
            message: Some("hello #{session_name}".to_owned()),
        }))
        .await;

    assert_eq!(
        response,
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );
    let overlay = control_rx.try_recv().expect("overlay control");
    let AttachControl::Overlay(overlay) = overlay else {
        panic!("expected display-message overlay");
    };
    let frame = String::from_utf8(overlay.frame).expect("overlay is utf-8");
    assert!(frame.contains("hello alpha"));
    assert!(frame.contains("\u{1b}[4;1H"));
}

#[tokio::test]
async fn display_message_uses_display_time_option_for_overlay_clear() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Session(alpha.clone()),
                OptionName::DisplayTime,
                "25".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set display-time");
    }
    handler.register_attach(43, alpha.clone(), control_tx).await;

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Session(alpha)),
            print: false,
            message: Some("quick clear".to_owned()),
        }))
        .await;

    assert_eq!(
        response,
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );

    let first = recv_overlay_control(&mut control_rx).await;
    let AttachControl::Overlay(first) = first else {
        panic!("expected display-message overlay");
    };
    let first_frame = String::from_utf8(first.frame).expect("overlay is utf-8");
    assert!(first_frame.contains("quick clear"));

    let second = timeout(
        Duration::from_millis(250),
        recv_overlay_control(&mut control_rx),
    )
    .await
    .expect("clear overlay should arrive within display-time");
    let AttachControl::Overlay(second) = second else {
        panic!("expected display-message clear overlay");
    };
    let second_frame = String::from_utf8(second.frame).expect("overlay is utf-8");
    assert!(!second_frame.contains("quick clear"));
}
