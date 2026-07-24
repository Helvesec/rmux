use super::attach_support::TransientMessageInput;
use super::RequestHandler;
use crate::pane_io::AttachControl;
use rmux_proto::{
    DisplayMessageExtRequest, DisplayMessageRequest, NewSessionRequest, NewWindowRequest,
    OptionName, OptionScopeSelector, PaneTarget, Request, Response, ScopeSelector,
    SelectPaneMarkRequest, SelectWindowRequest, SessionName, SetOptionMode, SetOptionRequest,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, Target, TerminalSize, WindowTarget,
};
#[cfg(windows)]
use std::path::Path;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

#[path = "handler_display_message_tests/pane_base_index.rs"]
mod pane_base_index;
#[path = "handler_display_message_tests/synchronize_panes.rs"]
mod synchronize_panes;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[cfg(unix)]
fn default_shell_window_name() -> String {
    "bash".to_owned()
}

#[cfg(windows)]
fn default_shell_window_name() -> String {
    std::env::var_os("COMSPEC")
        .and_then(|shell| Path::new(&shell).file_name().map(|name| name.to_owned()))
        .map(|name| name.to_string_lossy().trim_start_matches('-').to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "cmd.exe".to_owned())
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

/// Kills the helper process when the test ends, pass or fail.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const REQUESTER_ENVIRONMENT_HELPER: &str = "RMUX_TEST_REQUESTER_ENVIRONMENT_HELPER";

#[test]
fn requester_environment_probe_helper() {
    if std::env::var_os(REQUESTER_ENVIRONMENT_HELPER).is_some() {
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
}

/// Spawns a quiet, long-lived real process carrying the given environment so
/// the daemon-side requester-environment probe reads a foreign process environment —
/// the same mechanism a client-less CLI invocation exercises.
fn spawn_requester_with_environment(vars: &[(&str, String)]) -> ChildGuard {
    let executable = std::env::current_exe().expect("current test executable");
    let mut command = std::process::Command::new(executable);
    command.args([
        "--exact",
        "handler::display_message_tests::requester_environment_probe_helper",
        "--test-threads=1",
    ]);
    command.env(REQUESTER_ENVIRONMENT_HELPER, "1");
    command.env_remove("RMUX_PANE");
    command.env_remove("TMUX_PANE");
    for (key, value) in vars {
        command.env(key, value);
    }
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    ChildGuard(command.spawn().expect("spawn quiet requester process"))
}

#[tokio::test]
async fn client_less_display_message_prefers_requester_tmux_pane_over_attached_client() {
    // Issue #83: a client-less `display-message -p '#S'` run from inside a
    // pane of detached session B must resolve #S to B via the requester's
    // TMUX_PANE environment, not to the session of some other attached
    // client. The requester is a real child process so the daemon reads its
    // environment exactly as it would for a CLI invocation.
    let handler = RequestHandler::new();
    let socket_path =
        std::env::temp_dir().join(format!("rmux-issue83-{}.sock", std::process::id()));
    handler.set_socket_path(&socket_path);

    let attached = session_name("issue83-attached");
    let detached = session_name("issue83-detached");
    for name in [&attached, &detached] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: name.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&detached)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id().as_u32())
            .expect("detached session pane exists")
    };

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), attached.clone(), control_tx)
        .await;

    let expected_rmux = format!("{},1,0", socket_path.display());
    let expected_tmux_pane = format!("%{pane_id}");
    let requester = spawn_requester_with_environment(&[
        ("RMUX", expected_rmux.clone()),
        ("TMUX_PANE", expected_tmux_pane.clone()),
    ]);
    let requester_pid = requester.0.id();
    // Linux may expose the posix_spawn child in /proc before exec has
    // installed the requested environment. Wait for the exact fixture values
    // rather than accepting an empty or inherited pre-exec snapshot.
    let requester_environment = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(environment) = rmux_os::process::environment(requester_pid) {
                let ready = environment.get("TMUX_PANE") == Some(&expected_tmux_pane)
                    && environment.get("RMUX") == Some(&expected_rmux);
                if ready {
                    break environment;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("requester process installs its expected environment");
    assert_eq!(
        requester_environment.get("TMUX_PANE"),
        Some(&expected_tmux_pane),
        "requester fixture carries the detached pane identity"
    );
    assert_eq!(
        requester_environment.get("RMUX"),
        Some(&expected_rmux),
        "requester fixture carries the matching server socket"
    );

    let outcome = handler
        .dispatch(
            requester_pid,
            Request::DisplayMessage(DisplayMessageRequest {
                target: None,
                print: true,
                message: Some("#S".to_owned()),
                empty_target_context: false,
            }),
        )
        .await;
    let Response::DisplayMessage(response) = outcome.response else {
        panic!("expected display-message response: {:?}", outcome.response);
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(
        String::from_utf8_lossy(output.stdout()),
        format!("{detached}\n"),
        "client-less #S must follow the requester's TMUX_PANE, not the attached client"
    );
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
            empty_target_context: false,
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
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("detached".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::new(alpha, 0))),
            print: true,
            message: Some("#{active_window_index}:#{last_window_index}".to_owned()),
            empty_target_context: false,
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
async fn display_message_reports_session_and_window_stack_order() {
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

    for index in 1..=2 {
        assert!(matches!(
            handler
                .handle(Request::NewWindow(Box::new(NewWindowRequest {
                    target: alpha.clone(),
                    name: Some(format!("w{index}")),
                    detached: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    process_command: None,
                    target_window_index: Some(index),
                    insert_at_target: false,
                })))
                .await,
            Response::NewWindow(_)
        ));
    }

    for index in [0, 2] {
        assert!(matches!(
            handler
                .handle(Request::SelectWindow(SelectWindowRequest {
                    target: WindowTarget::with_window(alpha.clone(), index),
                }))
                .await,
            Response::SelectWindow(_)
        ));
    }

    for (window_index, expected_index) in [(2, "0"), (0, "1"), (1, "2")] {
        let response = handler
            .handle(Request::DisplayMessage(DisplayMessageRequest {
                target: Some(Target::Pane(PaneTarget::with_window(
                    alpha.clone(),
                    window_index,
                    0,
                ))),
                print: true,
                message: Some("#{session_stack}:#{window_stack_index}".to_owned()),
                empty_target_context: false,
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
            format!("2,0,1:{expected_index}\n").as_bytes()
        );
    }
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
                before: false,
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
            empty_target_context: false,
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
            empty_target_context: false,
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
            empty_target_context: false,
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

    #[cfg(unix)]
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                "/bin/bash".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("test default-shell is valid");
    }

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
            empty_target_context: false,
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

#[cfg(windows)]
#[tokio::test]
async fn display_message_print_uses_osc7_path_on_windows() {
    let handler = RequestHandler::new();
    let alpha = session_name("osc7cwd");

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

    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let expected_path = std::env::temp_dir().join("rmux osc7 cwd").join("pane");
    let expected = expected_path.to_string_lossy().into_owned();
    let uri_path = expected.replace('\\', "/").replace(' ', "%20");
    let osc7 = format!("\x1b]7;file:///{uri_path}\x1b\\");

    {
        let mut state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("pane exists");
        state
            .append_bytes_to_runtime_pane_transcript(&alpha, pane_id, osc7.as_bytes())
            .expect("OSC7 bytes append to pane transcript");
    }

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some("#{pane_current_path}".to_owned()),
            empty_target_context: false,
        }))
        .await;

    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), format!("{expected}\n").as_bytes());
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
                before: false,
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
            empty_target_context: false,
        }))
        .await;
    let pane1 = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 1))),
            print: true,
            message: Some(format.to_owned()),
            empty_target_context: false,
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
            empty_target_context: false,
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
                before: false,
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
            empty_target_context: false,
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
            message: Some(
                "#{S:#W}|#{W:#W,[#W]}|#{P:#{pane_index},[#{pane_index}]}|#{N:#W}".to_owned(),
            ),
            empty_target_context: false,
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
        format!("{window_name}|[{window_name}]|0[1]|1\n").as_bytes()
    );
}

#[tokio::test]
async fn display_message_session_loop_keeps_comma_body() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

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
            .handle(Request::NewSession(NewSessionRequest {
                session_name: beta,
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
            message: Some("#{S:#{session_name},CURRENT}".to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    assert_eq!(
        response
            .command_output()
            .expect("display-message -p returns output")
            .stdout(),
        b"alpha,CURRENTbeta,CURRENT\n"
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
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("w1".to_owned()),
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));

    let name_exists = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))),
            print: true,
            message: Some("#{N:#W}|#{N/w:w1}|#{N/s:alpha}|#{N/s:missing}".to_owned()),
            empty_target_context: false,
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
async fn display_message_content_search_modifier_reports_visible_line() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 8 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(
                &alpha,
                0,
                0,
                b"\x1b[H\x1b[2Jalpha one\r\nNeedle two\r\nlast row",
            )
            .expect("transcript append succeeds");
    }

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha, 0, 0))),
            print: true,
            message: Some(
                "#{C:alpha}|#{C:Needle}|#{C:absent}|#{C/i:needle}|#{C/r:N.*le}".to_owned(),
            ),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response for content search modifier");
    };
    assert_eq!(
        response
            .command_output()
            .expect("display-message -p returns output")
            .stdout(),
        b"1|2|0|2|2\n"
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
            empty_target_context: false,
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
            empty_target_context: false,
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
async fn display_message_target_client_delivers_only_to_that_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();

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
    handler.register_attach(42, alpha.clone(), first_tx).await;
    handler.register_attach(43, alpha, second_tx).await;

    let response = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: None,
                print: false,
                message: Some("for second".to_owned()),
                target_client: Some("43".to_owned()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )))
        .await;

    assert_eq!(
        response,
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );
    assert!(first_rx.try_recv().is_err());
    let overlay = second_rx.try_recv().expect("targeted overlay control");
    let AttachControl::Overlay(overlay) = overlay else {
        panic!("expected display-message overlay");
    };
    let frame = String::from_utf8(overlay.frame).expect("overlay is utf-8");
    assert!(frame.contains("for second"));
}

#[tokio::test]
async fn queued_display_message_ignore_input_is_scoped_to_its_attached_initiator() {
    // Oracle: tmux 3.7b with two clients attached to one session arms
    // `display-message -d2000 -N` only on the client that invoked the binding;
    // the peer continues to receive input.
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let initiator_pid = 44;
    let peer_pid = 45;
    let (initiator_tx, mut initiator_rx) = mpsc::unbounded_channel();
    let (peer_tx, mut peer_rx) = mpsc::unbounded_channel();
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
    handler
        .register_attach(initiator_pid, alpha.clone(), initiator_tx)
        .await;
    handler
        .register_attach(peer_pid, alpha.clone(), peer_tx)
        .await;
    let identity = handler
        .active_attach_identity(initiator_pid)
        .await
        .expect("initiating attached identity");
    let commands = handler
        .parse_control_commands("display-message -d 200 -N initiator-only")
        .await
        .expect("queued display-message parses");

    super::with_expected_attach_and_session_identity(
        identity,
        alpha,
        identity.session_id(),
        handler.execute_parsed_commands_for_test(initiator_pid, commands),
    )
    .await
    .expect("queued display-message executes");

    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut initiator_rx).await else {
        panic!("initiator receives its status message");
    };
    assert!(String::from_utf8_lossy(&overlay.frame).contains("initiator-only"));
    assert!(
        peer_rx.try_recv().is_err(),
        "a peer client must not receive or inherit the initiator's message"
    );
    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach
        .by_pid
        .get(&initiator_pid)
        .is_some_and(|active| active.transient_message.is_some()));
    assert!(active_attach
        .by_pid
        .get(&peer_pid)
        .is_some_and(|active| active.transient_message.is_none()));
}

#[tokio::test]
async fn direct_display_message_delivers_to_recent_client_but_formats_for_target_session() {
    // Oracle: an external tmux 3.7b command delivers to the most recently
    // active client, but formats client variables from a client attached to
    // the `-t` session when one exists.
    let handler = RequestHandler::new();
    let alpha = session_name("format-alpha");
    let beta = session_name("display-beta");
    for session_name in [alpha.clone(), beta.clone()] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 100, rows: 4 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }
    let alpha_pid = u32::MAX - 100;
    let beta_pid = u32::MAX - 99;
    let (alpha_tx, mut alpha_rx) = mpsc::unbounded_channel();
    let (beta_tx, mut beta_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(alpha_pid, alpha.clone(), alpha_tx)
        .await;
    handler
        .register_attach(beta_pid, beta.clone(), beta_tx)
        .await;

    let response = handler
        .handle_display_message_ext(
            7_777,
            DisplayMessageExtRequest {
                target: Some(Target::Session(alpha)),
                print: false,
                message: Some(
                    "format=#{session_name}|client=#{client_session}|name=#{client_name}"
                        .to_owned(),
                ),
                target_client: None,
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(5_000)),
                ignore_input: true,
            },
        )
        .await;
    assert!(matches!(response, Response::DisplayMessage(_)));
    assert!(alpha_rx.try_recv().is_err());
    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut beta_rx).await else {
        panic!("recent beta client receives the message");
    };
    assert!(String::from_utf8_lossy(&overlay.frame).contains(&format!(
        "format=format-alpha|client=format-alpha|name={alpha_pid}"
    )));
    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach
        .by_pid
        .get(&alpha_pid)
        .is_some_and(|active| active.transient_message.is_none()));
    assert!(active_attach
        .by_pid
        .get(&beta_pid)
        .is_some_and(|active| active.transient_message.is_some()));
    drop(active_attach);

    let response = handler
        .handle_display_message_ext(
            7_778,
            DisplayMessageExtRequest {
                target: Some(Target::Session(session_name("format-alpha"))),
                print: true,
                message: Some("#{session_name}|#{client_session}|#{client_name}".to_owned()),
                target_client: Some(beta_pid.to_string()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )
        .await;
    assert_eq!(
        response.command_output().map(|output| output.stdout()),
        Some(format!("format-alpha|format-alpha|{alpha_pid}\n").as_bytes())
    );

    assert!(handler
        .active_attach
        .lock()
        .await
        .record_client_activity(alpha_pid));
    let response = handler
        .handle_display_message_ext(
            7_778,
            DisplayMessageExtRequest {
                target: Some(Target::Session(session_name("format-alpha"))),
                print: false,
                message: Some("active alpha".to_owned()),
                target_client: None,
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(5_000)),
                ignore_input: true,
            },
        )
        .await;
    assert!(matches!(response, Response::DisplayMessage(_)));
    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut alpha_rx).await else {
        panic!("most recently active alpha client receives the next message");
    };
    assert!(String::from_utf8_lossy(&overlay.frame).contains("active alpha"));
}

#[tokio::test]
async fn direct_display_message_target_client_does_not_replace_current_format_client() {
    // Oracle: tmux 3.7b treats `-c` as the delivery client. Without `-t`,
    // formats still use the command client's current context.
    let handler = RequestHandler::new();
    let alpha = session_name("direct-delivery-alpha");
    let beta = session_name("direct-format-beta");
    let detached = session_name("direct-target-detached");
    for session_name in [alpha.clone(), beta.clone(), detached.clone()] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 100, rows: 4 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }
    let alpha_pid = u32::MAX - 98;
    let beta_pid = u32::MAX - 97;
    let (alpha_tx, mut alpha_rx) = mpsc::unbounded_channel();
    let (beta_tx, mut beta_rx) = mpsc::unbounded_channel();
    handler.register_attach(alpha_pid, alpha, alpha_tx).await;
    handler.register_attach(beta_pid, beta, beta_tx).await;

    let template = "#{session_name}|#{client_session}|#{client_name}";
    let response = handler
        .handle_display_message_ext(
            7_779,
            DisplayMessageExtRequest {
                target: None,
                print: true,
                message: Some(template.to_owned()),
                target_client: Some(alpha_pid.to_string()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )
        .await;
    assert_eq!(
        response.command_output().map(|output| output.stdout()),
        Some(format!("direct-format-beta|direct-format-beta|{beta_pid}\n").as_bytes())
    );

    let response = handler
        .handle_display_message_ext(
            7_780,
            DisplayMessageExtRequest {
                target: Some(Target::Session(detached.clone())),
                print: true,
                message: Some(template.to_owned()),
                target_client: Some(alpha_pid.to_string()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )
        .await;
    assert_eq!(
        response.command_output().map(|output| output.stdout()),
        Some(format!("{detached}|direct-format-beta|{beta_pid}\n").as_bytes())
    );

    let response = handler
        .handle_display_message_ext(
            7_781,
            DisplayMessageExtRequest {
                target: None,
                print: false,
                message: Some(template.to_owned()),
                target_client: Some(alpha_pid.to_string()),
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(5_000)),
                ignore_input: false,
            },
        )
        .await;
    assert!(matches!(response, Response::DisplayMessage(_)));
    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut alpha_rx).await else {
        panic!("explicit delivery client receives the message");
    };
    assert!(String::from_utf8_lossy(&overlay.frame)
        .contains(&format!("direct-format-beta|direct-format-beta|{beta_pid}")));
    assert!(
        beta_rx.try_recv().is_err(),
        "format client must not become the delivery client"
    );
}

#[tokio::test]
async fn queued_display_message_cross_client_detached_target_keeps_initiator_format_context() {
    // Oracle: from client A, `display-message -c B -t detached` delivers to B
    // but falls back to A for client formats because the target has no client.
    let handler = RequestHandler::new();
    let alpha = session_name("queued-format-alpha");
    let beta = session_name("queued-delivery-beta");
    let detached = session_name("queued-target-detached");
    for session_name in [alpha.clone(), beta.clone(), detached.clone()] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 100, rows: 4 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }
    let initiator_pid = u32::MAX - 96;
    let delivery_pid = u32::MAX - 95;
    let (initiator_tx, mut initiator_rx) = mpsc::unbounded_channel();
    let (delivery_tx, mut delivery_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(initiator_pid, alpha.clone(), initiator_tx)
        .await;
    handler
        .register_attach(delivery_pid, beta, delivery_tx)
        .await;
    let initiator = handler
        .active_attach_identity(initiator_pid)
        .await
        .expect("initiating attached identity");
    let template = "#{session_name}|#{client_session}|#{client_name}";

    let commands = handler
        .parse_control_commands(&format!(
            "display-message -p -c {delivery_pid} -t {detached} '{template}'"
        ))
        .await
        .expect("queued printed display-message parses");
    let output = super::with_expected_attach_and_session_identity(
        initiator,
        alpha.clone(),
        initiator.session_id(),
        handler.execute_parsed_commands_for_test(initiator_pid, commands),
    )
    .await
    .expect("queued printed display-message executes");
    assert_eq!(
        output.stdout(),
        format!("{detached}|{alpha}|{initiator_pid}\n").as_bytes()
    );

    let commands = handler
        .parse_control_commands(&format!(
            "display-message -d 5000 -c {delivery_pid} -t {detached} '{template}'"
        ))
        .await
        .expect("queued overlay display-message parses");
    super::with_expected_attach_and_session_identity(
        initiator,
        alpha.clone(),
        initiator.session_id(),
        handler.execute_parsed_commands_for_test(initiator_pid, commands),
    )
    .await
    .expect("queued overlay display-message executes");
    assert!(
        initiator_rx.try_recv().is_err(),
        "initiator supplies format context but does not receive the message"
    );
    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut delivery_rx).await else {
        panic!("explicit delivery client receives the queued message");
    };
    assert!(String::from_utf8_lossy(&overlay.frame)
        .contains(&format!("{detached}|{alpha}|{initiator_pid}")));
}

#[tokio::test]
async fn stable_pane_run_shell_message_broadcasts_only_to_the_target_session() {
    // Oracle: tmux 3.7b delivers targeted run-shell output to every client
    // attached to the target session, and to no client in another session.
    let handler = RequestHandler::new();
    let alpha = session_name("run-shell-output-alpha");
    let beta = session_name("run-shell-output-beta");
    for session_name in [alpha.clone(), beta.clone()] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 20, rows: 4 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }
    let pane_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .and_then(rmux_core::Session::active_pane_id)
        .expect("alpha active pane");
    let (alpha_one_tx, mut alpha_one_rx) = mpsc::unbounded_channel();
    let (alpha_two_tx, mut alpha_two_rx) = mpsc::unbounded_channel();
    let (beta_tx, mut beta_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(51, alpha.clone(), alpha_one_tx)
        .await;
    handler
        .register_attach(52, alpha.clone(), alpha_two_tx)
        .await;
    handler.register_attach(53, beta, beta_tx).await;

    let response = handler
        .handle_display_message_for_stable_pane(
            7_777,
            pane_id,
            DisplayMessageRequest {
                target: Some(Target::Session(alpha)),
                print: false,
                message: Some("RS".to_owned()),
                empty_target_context: false,
            },
        )
        .await;
    assert!(matches!(response, Response::DisplayMessage(_)));
    for receiver in [&mut alpha_one_rx, &mut alpha_two_rx] {
        let AttachControl::Overlay(overlay) = recv_overlay_control(receiver).await else {
            panic!("target-session client receives the message");
        };
        assert!(String::from_utf8_lossy(&overlay.frame).contains("RS"));
    }
    assert!(
        beta_rx.try_recv().is_err(),
        "another session must not receive targeted run-shell output"
    );
}

#[tokio::test]
async fn display_message_missing_target_client_is_noop_unless_printing() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let response = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: None,
                print: false,
                message: Some("hidden".to_owned()),
                target_client: Some("999999".to_owned()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )))
        .await;
    assert_eq!(
        response,
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );

    let response = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: None,
                print: true,
                message: Some("hello".to_owned()),
                target_client: Some("999999".to_owned()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )))
        .await;
    assert_eq!(
        response.command_output().map(|output| output.stdout()),
        Some(b"hello\n".as_slice())
    );
}

#[tokio::test]
async fn display_message_target_client_uses_client_session_for_overlay_delivery() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();

    for session_name in [alpha.clone(), beta.clone()] {
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 20, rows: 4 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
    }
    handler.register_attach(42, alpha, control_tx).await;

    let response = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(beta)),
                print: false,
                message: Some("format #{session_name} #{client_session}".to_owned()),
                target_client: Some("42".to_owned()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            },
        )))
        .await;

    assert_eq!(
        response,
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );
    let overlay = control_rx.try_recv().expect("targeted overlay control");
    let AttachControl::Overlay(overlay) = overlay else {
        panic!("expected display-message overlay");
    };
    let frame = String::from_utf8(overlay.frame).expect("overlay is utf-8");
    assert!(frame.contains("format beta alpha"));
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
            empty_target_context: false,
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

#[tokio::test]
async fn display_message_zero_delay_waits_for_input_and_forwards_that_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let attach_pid = 44;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;

    let response = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(alpha)),
                print: false,
                message: Some("wait for input".to_owned()),
                target_client: None,
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(0)),
                ignore_input: true,
            },
        )))
        .await;
    assert!(matches!(response, Response::DisplayMessage(_)));
    let _overlay = recv_overlay_control(&mut control_rx).await;
    assert!(
        timeout(Duration::from_millis(20), control_rx.recv())
            .await
            .is_err(),
        "zero delay must not arm a clear timer"
    );

    let identity = handler
        .active_attach_identity(attach_pid)
        .await
        .expect("attached identity");
    let mut pending = Vec::new();
    assert!(matches!(
        handler
            .handle_transient_message_input_for_identity(identity, &mut pending, b"x")
            .await,
        TransientMessageInput::Dismissed(bytes) if bytes == b"x"
    ));
    assert!(pending.is_empty());
    let AttachControl::Overlay(clear) = recv_overlay_control(&mut control_rx).await else {
        panic!("input dismissal must clear the message");
    };
    assert!(!String::from_utf8_lossy(&clear.frame).contains("wait for input"));
}

#[tokio::test]
async fn display_message_zero_display_time_does_not_enable_ignore_input() {
    // Oracle: tmux 3.7b treats `display-time 0; display-message -N` like
    // `display-message -d0 -N`: no timer, and the first key dismisses and is
    // forwarded.
    let handler = RequestHandler::new();
    let alpha = session_name("zero-display-time");
    let attach_pid = 48;
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
                "0".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("zero display-time");
    }
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(alpha)),
                print: false,
                message: Some("wait for key".to_owned()),
                target_client: Some(attach_pid.to_string()),
                empty_target_context: false,
                duration_ms: None,
                ignore_input: true,
            },
        )))
        .await;
    let _overlay = recv_overlay_control(&mut control_rx).await;
    assert!(
        timeout(Duration::from_millis(20), control_rx.recv())
            .await
            .is_err(),
        "display-time zero must not create an immediate expiry task"
    );
    let identity = handler
        .active_attach_identity(attach_pid)
        .await
        .expect("attached identity");
    let mut pending = Vec::new();
    assert!(matches!(
        handler
            .handle_transient_message_input_for_identity(identity, &mut pending, b"x")
            .await,
        TransientMessageInput::Dismissed(bytes) if bytes == b"x"
    ));
}

#[tokio::test]
async fn display_message_ignore_input_swallows_keys_until_positive_delay_expires() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let attach_pid = 45;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(alpha)),
                print: false,
                message: Some("ignore input".to_owned()),
                target_client: None,
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(40)),
                ignore_input: true,
            },
        )))
        .await;
    let _overlay = recv_overlay_control(&mut control_rx).await;

    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"x")
        .await
        .expect("ignored attached input succeeds");
    assert!(pending.is_empty());
    assert!(handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .is_some_and(|active| active.transient_message.is_some()));
    assert!(
        timeout(Duration::from_millis(10), control_rx.recv())
            .await
            .is_err(),
        "ignored input must not dismiss the message"
    );
    let AttachControl::Overlay(_) = timeout(
        Duration::from_millis(250),
        recv_overlay_control(&mut control_rx),
    )
    .await
    .expect("positive delay must expire") else {
        panic!("message expiry must emit an overlay clear");
    };
    assert!(handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .is_some_and(|active| active.transient_message.is_none()));
}

#[tokio::test]
async fn display_message_expiry_preserves_an_open_bracketed_paste_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("message-paste-expiry");
    let attach_pid = 45_001;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let target = PaneTarget::new(alpha.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&target);
    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(alpha)),
                print: false,
                message: Some("ignore paste prefix".to_owned()),
                target_client: Some(attach_pid.to_string()),
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(40)),
                ignore_input: true,
            },
        )))
        .await;
    let _message = recv_overlay_control(&mut control_rx).await;

    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"\x1b[200~BEFORE\x1b[20")
        .await
        .expect("paste prefix is ignored while the message is active");
    let _expiry = timeout(
        Duration::from_millis(250),
        recv_overlay_control(&mut control_rx),
    )
    .await
    .expect("message expires");
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"1~AFTER")
        .await
        .expect("paste suffix resumes after message expiry");
    assert!(pending.is_empty());
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .expect("pane input capture remains active"),
        b"AFTER"
    );
}

#[tokio::test]
async fn rearmed_display_message_preserves_an_expired_open_paste_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("message-paste-expiry-rearm");
    let attach_pid = 45_002;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let target = PaneTarget::new(alpha.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&target);

    for message in ["first ignore", "replacement ignore"] {
        assert!(matches!(
            handler
                .handle(Request::DisplayMessageExt(Box::new(
                    DisplayMessageExtRequest {
                        target: Some(Target::Session(alpha.clone())),
                        print: false,
                        message: Some(message.to_owned()),
                        target_client: Some(attach_pid.to_string()),
                        empty_target_context: false,
                        duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                        ignore_input: true,
                    },
                )))
                .await,
            Response::DisplayMessage(_)
        ));
        let _message = recv_overlay_control(&mut control_rx).await;
        if message == "first ignore" {
            let mut pending = Vec::new();
            handler
                .handle_attached_live_input(attach_pid, &mut pending, b"\x1b[200~BEFORE\x1b[20")
                .await
                .expect("first message owns the open paste");
            let (identity, overlay_generation) = {
                let active_attach = handler.active_attach.lock().await;
                let active = &active_attach.by_pid[&attach_pid];
                (
                    active.identity(attach_pid),
                    active
                        .transient_message
                        .as_ref()
                        .expect("first message remains active")
                        .overlay_generation(),
                )
            };
            handler
                .expire_transient_message_for_identity(identity, overlay_generation)
                .await;
        }
    }

    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"BODY-DURING-REPLACEMENT\n")
        .await
        .expect("replacement message keeps the inherited paste open");
    let (identity, overlay_generation) = {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&attach_pid];
        (
            active.identity(attach_pid),
            active
                .transient_message
                .as_ref()
                .expect("replacement message remains active")
                .overlay_generation(),
        )
    };
    handler
        .expire_transient_message_for_identity(identity, overlay_generation)
        .await;
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"\x1b[201~AFTER")
        .await
        .expect("post-expiry closer preserves the paste boundary");
    assert!(pending.is_empty());
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .expect("pane input capture remains active"),
        b"AFTER"
    );
}

#[tokio::test]
async fn display_message_ignore_input_preserves_outer_terminal_responses() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let attach_pid = 46;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let target = PaneTarget::new(alpha.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b]4;7;?\x1b\\")
            .expect("pane emits a palette query");
        state.start_pane_input_capture_for_test(&target);
    }
    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(alpha.clone())),
                print: false,
                message: Some("protocol response".to_owned()),
                target_client: None,
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                ignore_input: true,
            },
        )))
        .await;
    let _overlay = recv_overlay_control(&mut control_rx).await;

    let response = b"\x1b]4;7;rgb:1111/2222/3333\x1b\\";
    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, &response[..response.len() - 1])
        .await
        .expect("fragmented outer-terminal response is retained");
    assert!(
        pending.is_empty(),
        "the transient decoder owns the retained response while -N is active"
    );
    handler
        .handle_attached_live_input(attach_pid, &mut pending, &response[response.len() - 1..])
        .await
        .expect("outer terminal response is consumed by the protocol path");
    assert!(pending.is_empty());
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .expect("pane input capture remains active"),
        response
    );
    assert!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.transient_message.is_some()),
        "terminal responses must not dismiss or enter the message input policy"
    );
}

#[tokio::test]
async fn display_message_ignore_input_discards_generic_terminal_strings() {
    let handler = RequestHandler::new();
    let alpha = session_name("ignored-terminal-strings");
    let attach_pid = 4_612;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let target = PaneTarget::new(alpha.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&target);
    assert!(matches!(
        handler
            .handle(Request::DisplayMessageExt(Box::new(
                DisplayMessageExtRequest {
                    target: Some(Target::Session(alpha)),
                    print: false,
                    message: Some("ignore terminal strings".to_owned()),
                    target_client: Some(attach_pid.to_string()),
                    empty_target_context: false,
                    duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                    ignore_input: true,
                },
            )))
            .await,
        Response::DisplayMessage(_)
    ));
    let _message = recv_overlay_control(&mut control_rx).await;

    for terminal_string in [
        b"\x1bP1+rprefix=\x1b]52;c;bmVzdGVk\x07suffix\x1b\\".as_slice(),
        b"\x1b]133;unknown-osc\x07".as_slice(),
    ] {
        let split = terminal_string.len() / 2;
        let mut pending = Vec::new();
        handler
            .handle_attached_live_input(attach_pid, &mut pending, &terminal_string[..split])
            .await
            .expect("first terminal-string fragment is ignored");
        handler
            .handle_attached_live_input(attach_pid, &mut pending, &terminal_string[split..])
            .await
            .expect("complete terminal string remains ignored");
        assert!(pending.is_empty());
    }

    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .expect("pane input capture remains active"),
        b""
    );

    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"\x1bPignored-before-expiry")
        .await
        .expect("partial DCS is owned by the transient decoder");
    let (identity, overlay_generation) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attached client remains live");
        (
            active.identity(attach_pid),
            active
                .transient_message
                .as_ref()
                .expect("message remains active")
                .overlay_generation(),
        )
    };
    handler
        .expire_transient_message_for_identity(identity, overlay_generation)
        .await;
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"\x1b\\Z")
        .await
        .expect("post-expiry suffix is ordinary live input");
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .expect("pane input capture remains active"),
        b"\x1b\\Z",
        "tmux discards the pre-expiry DCS prefix and forwards only the later suffix"
    );
}

#[tokio::test]
async fn status_refresh_does_not_cover_an_active_display_message() {
    for (name, duration_ms, ignore_input) in [
        ("status-refresh-zero", 0, false),
        ("status-refresh-ignore", 10_000, true),
    ] {
        let handler = RequestHandler::new();
        let session = session_name(name);
        let attach_pid = if ignore_input { 4_602 } else { 4_601 };
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 20, rows: 4 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
        handler
            .register_attach(attach_pid, session.clone(), control_tx)
            .await;
        assert!(matches!(
            handler
                .handle(Request::DisplayMessageExt(Box::new(
                    DisplayMessageExtRequest {
                        target: Some(Target::Session(session.clone())),
                        print: false,
                        message: Some("must remain visible".to_owned()),
                        target_client: Some(attach_pid.to_string()),
                        empty_target_context: false,
                        duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(
                            duration_ms,
                        )),
                        ignore_input,
                    },
                )))
                .await,
            Response::DisplayMessage(_)
        ));
        let _message = recv_overlay_control(&mut control_rx).await;

        handler
            .refresh_attached_client_status(attach_pid, &session)
            .await
            .expect("status refresh succeeds while the message is visible");
        assert!(
            timeout(Duration::from_millis(20), control_rx.recv())
                .await
                .is_err(),
            "status refresh must not write over an active message"
        );
        assert!(handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.transient_message.is_some()));
    }
}

#[tokio::test]
async fn display_message_initial_frame_uses_the_target_clients_geometry() {
    let handler = RequestHandler::new();
    let session = session_name("message-client-geometry");
    let attach_pid = 4_601;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let client_size = TerminalSize { cols: 40, rows: 8 };
    {
        let mut active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get_mut(&attach_pid)
            .expect("attached client exists")
            .client_size = client_size;
    }
    let message = "CLIENT-GEOMETRY-MESSAGE";
    assert!(matches!(
        handler
            .handle(Request::DisplayMessageExt(Box::new(
                DisplayMessageExtRequest {
                    target: Some(Target::Session(session.clone())),
                    print: false,
                    message: Some(message.to_owned()),
                    target_client: Some(attach_pid.to_string()),
                    empty_target_context: false,
                    duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                    ignore_input: true,
                },
            )))
            .await,
        Response::DisplayMessage(_)
    ));
    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut control_rx).await else {
        panic!("display-message emits an overlay");
    };
    let (expected, wrong_geometry) = {
        let state = handler.state.lock().await;
        (
            super::attach_support::render_status_message_for_attached_size(
                &state,
                &session,
                client_size,
                message,
            )
            .expect("small status frame renders"),
            super::attach_support::render_status_message_for_attached_size(
                &state,
                &session,
                TerminalSize { cols: 80, rows: 24 },
                message,
            )
            .expect("canonical status frame renders"),
        )
    };
    assert!(overlay
        .frame
        .windows(expected.len())
        .any(|part| part == expected));
    assert!(
        !overlay
            .frame
            .windows(wrong_geometry.len())
            .any(|part| part == wrong_geometry),
        "the initial frame must not use the canonical session geometry"
    );
}

#[tokio::test]
async fn resize_rerenders_an_ignored_display_message_at_the_new_geometry() {
    let handler = RequestHandler::new();
    let session = session_name("message-resize-geometry");
    let attach_pid = 4_602;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let message = "MESSAGE-RENDERED-AFTER-RESIZE";
    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(session.clone())),
                print: false,
                message: Some(message.to_owned()),
                target_client: Some(attach_pid.to_string()),
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                ignore_input: true,
            },
        )))
        .await;
    let _message = recv_overlay_control(&mut control_rx).await;

    let resized = TerminalSize { cols: 40, rows: 8 };
    handler
        .handle_attached_resize(attach_pid, resized)
        .await
        .expect("attached resize succeeds");
    let refreshed = loop {
        let control = timeout(Duration::from_secs(2), control_rx.recv())
            .await
            .expect("resize refresh is timely")
            .expect("attach remains active");
        if let AttachControl::Switch(target) = control {
            break target.into_target();
        }
    };
    let expected = {
        let state = handler.state.lock().await;
        super::attach_support::render_status_message_for_attached_size(
            &state, &session, resized, message,
        )
        .expect("resized status frame renders")
    };
    assert!(
        refreshed
            .render_frame
            .windows(expected.len())
            .any(|part| part == expected),
        "the full resize refresh must preserve the message at the new geometry"
    );
}

#[tokio::test]
async fn closing_a_popup_repaints_an_ignored_display_message() {
    let handler = RequestHandler::new();
    let session = session_name("message-popup-restore");
    let attach_pid = 4_604;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 60, rows: 12 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let message = "MESSAGE-RESTORED-AFTER-POPUP";
    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(session.clone())),
                print: false,
                message: Some(message.to_owned()),
                target_client: Some(attach_pid.to_string()),
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                ignore_input: true,
            },
        )))
        .await;
    let _message = recv_overlay_control(&mut control_rx).await;

    let popup = handler
        .parse_control_commands("display-popup -N -E -T Popup -w 20 -h 6 -x C -y C")
        .await
        .expect("popup parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, popup)
        .await
        .expect("popup opens");
    let _popup = recv_overlay_control(&mut control_rx).await;
    handler
        .clear_interactive_overlay(attach_pid, true)
        .await
        .expect("popup closes");

    let restored = loop {
        let control = timeout(Duration::from_secs(2), control_rx.recv())
            .await
            .expect("popup restoration is timely")
            .expect("attach remains active");
        if let AttachControl::Switch(target) = control {
            break target.into_target();
        }
    };
    let client_size = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("attach remains active")
        .client_size;
    let expected = {
        let state = handler.state.lock().await;
        super::attach_support::render_status_message_for_attached_size(
            &state,
            &session,
            client_size,
            message,
        )
        .expect("restored status frame renders")
    };
    assert!(
        restored
            .render_frame
            .windows(expected.len())
            .any(|part| part == expected),
        "closing the popup must restore the active display message; frame={:?}",
        String::from_utf8_lossy(&restored.render_frame),
    );
}

#[tokio::test]
async fn display_message_composes_with_an_existing_popup() {
    let handler = RequestHandler::new();
    let session = session_name("popup-before-message");
    let attach_pid = 4_605;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 60, rows: 12 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let popup = handler
        .parse_control_commands("display-popup -N -E -T POPUP-SURFACE -w 20 -h 6 -x C -y C")
        .await
        .expect("popup parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, popup)
        .await
        .expect("popup opens");
    let _popup = recv_overlay_control(&mut control_rx).await;

    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(session)),
                print: false,
                message: Some("MESSAGE-OVER-POPUP".to_owned()),
                target_client: Some(attach_pid.to_string()),
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                ignore_input: true,
            },
        )))
        .await;
    let _initial_message = recv_overlay_control(&mut control_rx).await;
    let AttachControl::Overlay(composed) = recv_overlay_control(&mut control_rx).await else {
        panic!("popup is repainted after the message");
    };
    let rendered = String::from_utf8_lossy(&composed.frame);
    assert!(rendered.contains("POPUP-SURFACE"));
    assert!(rendered.contains("MESSAGE-OVER-POPUP"));
}

#[tokio::test]
async fn display_panes_composes_with_an_ignored_display_message() {
    let handler = RequestHandler::new();
    let session = session_name("message-with-display-panes");
    let attach_pid = 4_603;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 40, rows: 8 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let expected_labels = {
        let state = handler.state.lock().await;
        let session_state = state
            .sessions
            .session(&session)
            .expect("display-panes session exists");
        crate::renderer::render_display_panes_overlay(session_state, &state.options)
    };
    assert!(matches!(
        handler
            .handle(Request::DisplayMessageExt(Box::new(
                DisplayMessageExtRequest {
                    target: Some(Target::Session(session.clone())),
                    print: false,
                    message: Some("VISIBLE-MESSAGE".to_owned()),
                    target_client: Some(attach_pid.to_string()),
                    empty_target_context: false,
                    duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                    ignore_input: true,
                },
            )))
            .await,
        Response::DisplayMessage(_)
    ));
    let _message = recv_overlay_control(&mut control_rx).await;
    let display_panes = handler
        .parse_control_commands("display-panes -b -d 60000")
        .await
        .expect("display-panes parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, display_panes)
        .await
        .expect("display-panes starts");

    let AttachControl::Overlay(composed) = recv_overlay_control(&mut control_rx).await else {
        panic!("display-panes emits an overlay");
    };
    let rendered = String::from_utf8_lossy(&composed.frame);
    assert!(
        composed
            .frame
            .windows(expected_labels.len())
            .any(|window| window == expected_labels),
        "the complete pane-label overlay remains visible"
    );
    assert!(
        rendered.contains("VISIBLE-MESSAGE"),
        "display-panes must repaint the active status message"
    );

    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"0")
        .await
        .expect("ignored label input is handled");
    let active_attach = handler.active_attach.lock().await;
    let active = &active_attach.by_pid[&attach_pid];
    assert!(active.display_panes.is_some());
    assert!(active.transient_message.is_some());
}

#[tokio::test]
async fn display_message_composes_with_existing_display_panes() {
    let handler = RequestHandler::new();
    let session = session_name("display-panes-before-message");
    let attach_pid = 4_606;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 40, rows: 8 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let expected_labels = {
        let state = handler.state.lock().await;
        crate::renderer::render_display_panes_overlay(
            state.sessions.session(&session).expect("session exists"),
            &state.options,
        )
    };
    let display_panes = handler
        .parse_control_commands("display-panes -b -d 60000")
        .await
        .expect("display-panes parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, display_panes)
        .await
        .expect("display-panes starts");
    let _labels = recv_overlay_control(&mut control_rx).await;
    while control_rx.try_recv().is_ok() {}

    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(session)),
                print: false,
                message: Some("MESSAGE-OVER-LABELS".to_owned()),
                target_client: Some(attach_pid.to_string()),
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                ignore_input: true,
            },
        )))
        .await;
    let _initial_message = recv_overlay_control(&mut control_rx).await;
    let AttachControl::Overlay(composed) = recv_overlay_control(&mut control_rx).await else {
        panic!("display-panes is repainted after the message");
    };
    assert!(
        composed
            .frame
            .windows(expected_labels.len())
            .any(|window| window == expected_labels),
        "display-panes overlay missing; frame={:?}; expected={:?}",
        String::from_utf8_lossy(&composed.frame),
        String::from_utf8_lossy(&expected_labels),
    );
    assert!(String::from_utf8_lossy(&composed.frame).contains("MESSAGE-OVER-LABELS"));
}

#[tokio::test]
async fn expiring_message_restores_display_panes_and_popup_in_order() {
    let handler = RequestHandler::new();
    let session = session_name("restore-display-panes-and-popup");
    let attach_pid = 4_607;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 40, rows: 8 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let expected_labels = {
        let state = handler.state.lock().await;
        crate::renderer::render_display_panes_overlay(
            state.sessions.session(&session).expect("session exists"),
            &state.options,
        )
    };

    let display_panes = handler
        .parse_control_commands("display-panes -b -d 60000")
        .await
        .expect("display-panes parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, display_panes)
        .await
        .expect("display-panes starts");
    let _labels = recv_overlay_control(&mut control_rx).await;
    let popup = handler
        .parse_control_commands("display-popup -N -E -T RESTORED-POPUP -w 20 -h 6 -x C -y C")
        .await
        .expect("popup parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, popup)
        .await
        .expect("popup opens");
    let _popup = recv_overlay_control(&mut control_rx).await;

    assert!(matches!(
        handler
            .handle(Request::DisplayMessageExt(Box::new(
                DisplayMessageExtRequest {
                    target: Some(Target::Session(session.clone())),
                    print: false,
                    message: Some("TRANSIENT-OVER-BOTH".to_owned()),
                    target_client: Some(attach_pid.to_string()),
                    empty_target_context: false,
                    duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                    ignore_input: true,
                },
            )))
            .await,
        Response::DisplayMessage(_)
    ));
    let _message = recv_overlay_control(&mut control_rx).await;
    while control_rx.try_recv().is_ok() {}

    let (identity, overlay_generation) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attached client remains live");
        (
            active.identity(attach_pid),
            active
                .transient_message
                .as_ref()
                .expect("message remains active")
                .overlay_generation(),
        )
    };
    handler
        .expire_transient_message_for_identity(identity, overlay_generation)
        .await;

    let mut saw_labels = false;
    let mut saw_popup = false;
    for _ in 0..8 {
        let control = timeout(Duration::from_secs(1), control_rx.recv())
            .await
            .expect("restore emits the next frame")
            .expect("attach remains active");
        let AttachControl::Overlay(overlay) = control else {
            continue;
        };
        saw_labels |= overlay
            .frame
            .windows(expected_labels.len())
            .any(|window| window == expected_labels);
        saw_popup |= String::from_utf8_lossy(&overlay.frame).contains("RESTORED-POPUP");
        if saw_labels && saw_popup {
            break;
        }
    }
    assert!(saw_labels, "display-panes must be restored");
    assert!(
        saw_popup,
        "the topmost popup must be restored after display-panes"
    );

    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"\x1b")
        .await
        .expect("restored popup remains interactive");
    handler
        .flush_attached_pending_escape_input(attach_pid, &mut pending)
        .await
        .expect("restored popup handles Escape after escape-time");
    assert!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.overlay.is_none()),
        "Escape must close the restored popup rather than an invisible surface"
    );
}

#[tokio::test]
async fn older_display_message_timer_cannot_clear_a_newer_message() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let attach_pid = 47;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;

    for (message, delay) in [("old", 20), ("new", 120)] {
        let _ = handler
            .handle(Request::DisplayMessageExt(Box::new(
                DisplayMessageExtRequest {
                    target: Some(Target::Session(alpha.clone())),
                    print: false,
                    message: Some(message.to_owned()),
                    target_client: None,
                    empty_target_context: false,
                    duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(delay)),
                    ignore_input: false,
                },
            )))
            .await;
        let _overlay = recv_overlay_control(&mut control_rx).await;
    }
    assert!(
        timeout(Duration::from_millis(50), control_rx.recv())
            .await
            .is_err(),
        "the old timer must not clear the replacement message"
    );
    let AttachControl::Overlay(_) = timeout(
        Duration::from_millis(250),
        recv_overlay_control(&mut control_rx),
    )
    .await
    .expect("replacement message must eventually expire") else {
        panic!("replacement expiry must emit an overlay clear");
    };
}

#[tokio::test]
async fn dismissed_message_restore_cannot_overwrite_a_newer_persistent_surface() {
    let handler = RequestHandler::new();
    let alpha = session_name("transient-restore-generation");
    let attach_pid = 49;
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
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let _ = handler
        .handle(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: Some(Target::Session(alpha)),
                print: false,
                message: Some("old transient".to_owned()),
                target_client: Some(attach_pid.to_string()),
                empty_target_context: false,
                duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(0)),
                ignore_input: false,
            },
        )))
        .await;
    let _old_overlay = recv_overlay_control(&mut control_rx).await;
    let identity = handler
        .active_attach_identity(attach_pid)
        .await
        .expect("attached identity");
    let pause = super::attach_support::install_transient_restore_commit_pause(attach_pid);
    let dismiss_handler = handler.clone();
    let dismiss = tokio::spawn(async move {
        let mut pending = Vec::new();
        dismiss_handler
            .handle_transient_message_input_for_identity(identity, &mut pending, b"x")
            .await
    });
    timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("old restore reaches its final commit");

    {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .expect("attached client remains live");
        active.overlay_generation = active.overlay_generation.saturating_add(1);
        active
            .control_tx
            .send(AttachControl::Overlay(
                crate::pane_io::OverlayFrame::persistent(
                    b"new persistent surface".to_vec(),
                    active.render_generation,
                    active.overlay_generation,
                ),
            ))
            .expect("new persistent surface queues");
    }
    pause.release.notify_one();
    assert!(matches!(
        dismiss.await.expect("dismiss task joins"),
        TransientMessageInput::Dismissed(bytes) if bytes == b"x"
    ));

    let mut saw_new_surface = false;
    while let Ok(Some(control)) = timeout(Duration::from_millis(50), control_rx.recv()).await {
        match control {
            AttachControl::Overlay(overlay)
                if overlay.frame == b"new persistent surface".as_slice() =>
            {
                saw_new_surface = true;
            }
            AttachControl::Switch(_) if saw_new_surface => {
                panic!("stale transient restoration overwrote the newer surface");
            }
            _ => {}
        }
    }
    assert!(saw_new_surface, "new persistent surface was not observed");
}
