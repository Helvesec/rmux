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

#[path = "handler_display_message_tests/automatic_window_name.rs"]
mod automatic_window_name;
#[path = "handler_display_message_tests/cursor_visibility.rs"]
mod cursor_visibility;
#[path = "handler_display_message_tests/ignored_paste_boundary.rs"]
mod ignored_paste_boundary;
#[path = "handler_display_message_tests/pane_base_index.rs"]
mod pane_base_index;
#[path = "handler_display_message_tests/pane_tabs.rs"]
mod pane_tabs;
#[path = "handler_display_message_tests/scroll_region.rs"]
mod scroll_region;
#[path = "handler_display_message_tests/status_overlay.rs"]
mod status_overlay;
#[path = "handler_display_message_tests/synchronize_panes.rs"]
mod synchronize_panes;
#[path = "handler_display_message_tests/terminal_mode_flags.rs"]
mod terminal_mode_flags;
#[path = "handler_display_message_tests/terminal_policy.rs"]
mod terminal_policy;
#[path = "handler_display_message_tests/wrap1.rs"]
mod wrap1;

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
pub(in crate::handler) struct ChildGuard(pub(in crate::handler) std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const REQUESTER_ENVIRONMENT_HELPER: &str = "RMUX_TEST_REQUESTER_ENVIRONMENT_HELPER";
/// Printed by the helper from its own test body, after process start-up.
const REQUESTER_READY_LINE: &str = "rmux-test-requester-ready";
/// Directory in which a held helper writes `holding` from its own test body
/// and then stays short of its readiness line until `release` appears there.
pub(in crate::handler) const REQUESTER_HOLD_DIR: &str = "RMUX_TEST_REQUESTER_HOLD_DIR";

#[test]
fn requester_environment_probe_helper() {
    use std::io::Write;

    if std::env::var_os(REQUESTER_ENVIRONMENT_HELPER).is_some() {
        hold_requester_before_ready();
        // A direct stdout write bypasses libtest capture; the parent waits for it.
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{REQUESTER_READY_LINE}").expect("announce requester readiness");
        stdout.flush().expect("flush requester readiness");
        drop(stdout);
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
}

/// Models a requester whose start-up has not finished: its environment is
/// already readable, but it has not reached its readiness point.
fn hold_requester_before_ready() {
    let Some(hold) = std::env::var_os(REQUESTER_HOLD_DIR).map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::write(hold.join("holding"), b"").expect("record the held requester");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !hold.join("release").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Spawns a quiet, long-lived real process carrying the given environment so
/// the daemon-side requester-environment probe reads a foreign process environment —
/// the same mechanism a client-less CLI invocation exercises.
///
/// Returns only once the helper runs its own code. A CLI requester has always
/// done so before the daemon reads its environment, whereas a child still in
/// start-up gives no such guarantee: on Windows its environment, already
/// visible once, was missing again at the next read.
pub(in crate::handler) async fn spawn_requester_with_environment(
    vars: &[(&str, String)],
) -> ChildGuard {
    let mut requester = spawn_requester_process(vars);
    wait_for_requester_ready(&mut requester).await;
    requester
}

fn spawn_requester_process(vars: &[(&str, String)]) -> ChildGuard {
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
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    ChildGuard(command.spawn().expect("spawn quiet requester process"))
}

/// Reads the helper's stdout until its readiness marker; `false` means the
/// helper closed stdout first. libtest has already printed `test <name> ... `
/// on that line, so the marker ends the line rather than filling it.
fn read_requester_ready_line(stdout: std::process::ChildStdout) -> bool {
    use std::io::BufRead;

    std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .any(|line| line.ends_with(REQUESTER_READY_LINE))
}

async fn wait_for_requester_ready(requester: &mut ChildGuard) {
    let stdout = requester
        .0
        .stdout
        .take()
        .expect("requester stdout is piped");
    let reader = tokio::task::spawn_blocking(move || read_requester_ready_line(stdout));
    let ready = timeout(Duration::from_secs(30), reader)
        .await
        .expect("requester helper reaches its own code within 30 s")
        .expect("requester readiness reader");
    assert!(
        ready,
        "requester helper exited before reaching its own code"
    );
}

/// Describes the requester as the daemon would read it now, for failure messages.
fn requester_snapshot(requester: &mut ChildGuard) -> String {
    let pid = requester.0.id();
    let exit = requester.0.try_wait();
    let environment = rmux_os::process::environment(pid).map(|environment| {
        (
            environment.get("RMUX").cloned(),
            environment.get("RMUX_PANE").cloned(),
        )
    });
    format!("requester pid {pid}, exit {exit:?}, (RMUX, RMUX_PANE) now {environment:?}")
}

async fn wait_for_requester_environment(requester_pid: u32, expected: &[(&str, &str)]) {
    timeout(Duration::from_secs(2), async {
        loop {
            if let Some(environment) = rmux_os::process::environment(requester_pid) {
                if expected
                    .iter()
                    .all(|(name, value)| environment.get(*name).map(String::as_str) == Some(*value))
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("requester process installs its expected environment");
}

/// Native diagnostic, run explicitly with `--ignored --nocapture`. Counts the
/// reads in which a requester environment, once visible, is missing again,
/// separately before and after the requester runs its own code.
#[test]
#[ignore = "native diagnostic: requester environment visibility across start-up"]
fn requester_environment_visibility_probe() {
    use std::sync::mpsc::TryRecvError;
    use std::time::Instant;

    const CHILDREN: usize = 40;
    let after_ready = std::time::Duration::from_millis(200);
    let give_up = std::time::Duration::from_secs(30);
    let (mut lost_before_ready, mut lost_after_ready, mut lost_after_exit) = (0, 0, 0);
    let (mut children_with_loss, mut exited_before_ready, mut never_ready) = (0, 0, 0);
    for child_index in 0..CHILDREN {
        let marker = format!("visibility-probe-{}-{child_index}", std::process::id());
        let mut requester = spawn_requester_process(&[("RMUX", marker.clone())]);
        let pid = requester.0.id();
        let stdout = requester
            .0
            .stdout
            .take()
            .expect("requester stdout is piped");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = ready_tx.send(read_requester_ready_line(stdout));
        });
        let started = Instant::now();
        let (mut first_visible, mut ready_at, mut child_lost) = (None, None, 0);
        loop {
            let elapsed = started.elapsed();
            if ready_at.is_none() {
                match ready_rx.try_recv() {
                    Ok(true) => ready_at = Some(elapsed),
                    Ok(false) | Err(TryRecvError::Disconnected) => {
                        exited_before_ready += 1;
                        break;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            let visible = rmux_os::process::environment(pid)
                .is_some_and(|environment| environment.get("RMUX") == Some(&marker));
            if visible && first_visible.is_none() {
                first_visible = Some(elapsed);
            } else if !visible && first_visible.is_some() {
                child_lost += 1;
                if !matches!(requester.0.try_wait(), Ok(None)) {
                    lost_after_exit += 1;
                } else if ready_at.is_some() {
                    lost_after_ready += 1;
                } else {
                    lost_before_ready += 1;
                }
            }
            if ready_at.is_some_and(|ready| elapsed >= ready + after_ready) || elapsed >= give_up {
                break;
            }
            std::thread::yield_now();
        }
        if child_lost > 0 {
            children_with_loss += 1;
        }
        if ready_at.is_none() && started.elapsed() >= give_up {
            never_ready += 1;
        }
        println!(
            "visibility-probe child={child_index} pid={pid} first_visible={first_visible:?} \
             ready={ready_at:?} lost_reads={child_lost}"
        );
        drop(requester);
        reader.join().expect("requester readiness reader");
    }
    println!(
        "visibility-probe children={CHILDREN} lost_before_ready={lost_before_ready} \
         lost_after_ready={lost_after_ready} lost_after_exit={lost_after_exit} \
         children_with_loss={children_with_loss} exited_before_ready={exited_before_ready} \
         never_ready={never_ready}"
    );
    assert_eq!(
        (lost_after_ready, exited_before_ready, never_ready),
        (0, 0, 0),
        "a requester that runs its own code must keep a visible environment"
    );
}

/// Issue #83 fixture with two clients: an attached client on one session and
/// a client-less requester whose environment, built by `environment` from the
/// server socket, the detached session id and its pane (`%N`), names the pane
/// of a second, detached session. The requester is a real child process so
/// the daemon reads its environment exactly as it would for a CLI invocation.
/// Returns `display-message -p '#S'` with the attached and detached names.
async fn client_less_display_session(
    tag: &str,
    environment: impl FnOnce(&std::path::Path, u32, String) -> Vec<(&'static str, String)>,
) -> (String, SessionName, SessionName) {
    let handler = RequestHandler::new();
    let socket_path =
        std::env::temp_dir().join(format!("rmux-issue83-{tag}-{}.sock", std::process::id()));
    handler.set_socket_path(&socket_path);

    let attached = session_name("issue83-attached");
    let detached = session_name("issue83-detached");
    for name in [&attached, &detached] {
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
    }
    let (pane_id, session_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&detached)
            .expect("detached session exists");
        let pane_id = session
            .window_at(0)
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id().as_u32())
            .expect("detached session pane exists");
        (pane_id, session.id())
    };

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), attached.clone(), control_tx)
        .await;

    let vars = environment(&socket_path, session_id.as_u32(), format!("%{pane_id}"));
    let requester = spawn_requester_with_environment(&vars).await;
    let requester_pid = requester.0.id();
    // Linux may expose the posix_spawn child in /proc before exec has
    // installed the requested environment. Wait for the exact fixture values
    // rather than accepting an empty or inherited pre-exec snapshot.
    let requester_environment = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(environment) = rmux_os::process::environment(requester_pid) {
                if vars
                    .iter()
                    .all(|(key, value)| environment.get(*key) == Some(value))
                {
                    break environment;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("requester process installs its expected environment");
    for (key, value) in &vars {
        assert_eq!(
            requester_environment.get(*key),
            Some(value),
            "requester fixture carries {key}"
        );
    }

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
    (
        String::from_utf8_lossy(output.stdout()).into_owned(),
        attached,
        detached,
    )
}

#[tokio::test]
async fn client_less_display_message_prefers_requester_tmux_pane_over_attached_client() {
    // Issue #83: a client-less `display-message -p '#S'` run from inside a
    // pane of detached session B must resolve #S to B via the requester's
    // TMUX_PANE environment, not to the session of some other attached
    // client.
    let (output, _attached, detached) =
        client_less_display_session("rmux", |socket, session_id, pane| {
            vec![
                ("RMUX", format!("{},1,{session_id}", socket.display())),
                ("TMUX_PANE", pane),
            ]
        })
        .await;
    assert_eq!(
        output,
        format!("{detached}\n"),
        "client-less #S must follow the requester's TMUX_PANE, not the attached client"
    );
}

#[tokio::test]
async fn client_less_display_message_follows_tmux_pane_without_rmux() {
    // tmux 3.7b (cmd_find_inside_pane) reads TMUX_PANE from a requester
    // outside any pane, alone or beside a TMUX naming this server; RMUX is
    // not required.
    let (alone, _attached, detached) =
        client_less_display_session("tmux-pane", |_, _, pane| vec![("TMUX_PANE", pane)]).await;
    assert_eq!(
        alone,
        format!("{detached}\n"),
        "TMUX_PANE alone must name the pane, not the attached client"
    );
    let (with_tmux, _attached, detached) =
        client_less_display_session("tmux-same", |socket, session_id, pane| {
            vec![
                ("TMUX", format!("{},1,{session_id}", socket.display())),
                ("TMUX_PANE", pane),
            ]
        })
        .await;
    assert_eq!(
        with_tmux,
        format!("{detached}\n"),
        "TMUX naming this server with TMUX_PANE must name the pane"
    );
}

#[tokio::test]
async fn client_less_display_message_ignores_tmux_pane_of_another_server_product_divergence() {
    // Deliberate divergence from tmux 3.7b, which ignores the socket in TMUX:
    // a TMUX naming another server (rmux run inside a tmux pane) makes its
    // TMUX_PANE that server's pane, so the attached client still decides.
    let (output, attached, _detached) =
        client_less_display_session("tmux-other", |socket, _, pane| {
            vec![
                (
                    "TMUX",
                    format!("{},1,0", socket.with_file_name("other.sock").display()),
                ),
                ("TMUX_PANE", pane),
            ]
        })
        .await;
    assert_eq!(
        output,
        format!("{attached}\n"),
        "a TMUX_PANE of another server must not name a pane here"
    );
}

#[tokio::test]
async fn linked_display_message_uses_inherited_session_occurrence_and_refuses_stale_pair() {
    let handler = RequestHandler::new();
    let socket_path =
        std::env::temp_dir().join(format!("rmux-linked-display-{}.sock", std::process::id()));
    handler.set_socket_path(&socket_path);

    let primary = session_name("linked-display-primary");
    let linked = session_name("linked-display-other");
    let unrelated = session_name("linked-display-unrelated");
    for name in [&primary, &linked, &unrelated] {
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
    }
    let (primary_id, primary_window_id, pane_id, unrelated_id) = {
        let state = handler.state.lock().await;
        let primary_session = state.sessions.session(&primary).expect("primary session");
        let primary_window = primary_session.window_at(0).expect("primary window");
        let pane_id = primary_window.pane(0).expect("primary pane").id();
        let unrelated_id = state
            .sessions
            .session(&unrelated)
            .expect("unrelated session")
            .id();
        (
            primary_session.id(),
            primary_window.id(),
            pane_id,
            unrelated_id,
        )
    };
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(rmux_proto::LinkWindowRequest {
                source: WindowTarget::with_window(primary.clone(), 0),
                target: WindowTarget::with_window(linked, 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));

    let pane_environment = pane_id.to_string();
    let valid_rmux = format!("{},1,{}", socket_path.display(), primary_id.as_u32());
    let mut valid = spawn_requester_with_environment(&[
        ("RMUX", valid_rmux.clone()),
        ("RMUX_PANE", pane_environment.clone()),
    ])
    .await;
    wait_for_requester_environment(
        valid.0.id(),
        &[
            ("RMUX", valid_rmux.as_str()),
            ("RMUX_PANE", pane_environment.as_str()),
        ],
    )
    .await;
    let expected = format!("{primary}|{primary_id}|0|{primary_window_id}|{pane_id}\n");
    let response = handler
        .dispatch(
            valid.0.id(),
            Request::DisplayMessage(DisplayMessageRequest {
                target: None,
                print: true,
                message: Some(
                    "#{session_name}|#{session_id}|#{window_index}|#{window_id}|#{pane_id}"
                        .to_owned(),
                ),
                empty_target_context: false,
            }),
        )
        .await
        .response;
    assert!(
        matches!(response.command_output(), Some(output) if output.stdout() == expected.as_bytes()),
        "valid linked occurrence must be retained: {response:?}; {}",
        requester_snapshot(&mut valid)
    );

    let stale_rmux = format!("{},1,{}", socket_path.display(), unrelated_id.as_u32());
    let mut stale = spawn_requester_with_environment(&[
        ("RMUX", stale_rmux.clone()),
        ("RMUX_PANE", pane_environment.clone()),
    ])
    .await;
    wait_for_requester_environment(
        stale.0.id(),
        &[
            ("RMUX", stale_rmux.as_str()),
            ("RMUX_PANE", pane_environment.as_str()),
        ],
    )
    .await;
    let stale_response = handler
        .dispatch(
            stale.0.id(),
            Request::DisplayMessage(DisplayMessageRequest {
                target: None,
                print: true,
                message: Some("#{pane_id}".to_owned()),
                empty_target_context: false,
            }),
        )
        .await
        .response;
    assert!(
        matches!(stale_response, Response::Error(ref response)
            if response.error.to_string().contains("is not present in inherited session")),
        "stale inherited pair must not fall back: {stale_response:?}; {}",
        requester_snapshot(&mut stale)
    );

    let explicit = handler
        .dispatch(
            stale.0.id(),
            Request::DisplayMessage(DisplayMessageRequest {
                target: Some(Target::Pane(PaneTarget::with_window(primary, 0, 0))),
                print: true,
                message: Some("#{pane_id}".to_owned()),
                empty_target_context: false,
            }),
        )
        .await
        .response;
    assert!(
        matches!(explicit.command_output(), Some(output) if output.stdout() == format!("{pane_id}\n").as_bytes()),
        "explicit target must remain independent of stale implicit context: {explicit:?}"
    );
}

#[tokio::test]
async fn display_message_print_expands_shared_formats_without_attached_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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

    // Only the windows this session really visited are in the stack, and a
    // window it never left for another has rank 0 (`format.c:661-684`).
    for (window_index, expected_index) in [(2, "0"), (0, "1"), (1, "0")] {
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
            format!("2,0:{expected_index}\n").as_bytes()
        );
    }
}

#[tokio::test]
async fn display_message_print_uses_full_detached_geometry_for_window_and_pane_formats() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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
    let handler = RequestHandler::with_host_shell_for_test();
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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
async fn display_message_session_loop_keeps_comma_body() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: beta,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );

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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 8 }),
                environment: None,
            }))
            .await,
    );
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
async fn display_message_stale_explicit_client_does_not_fall_back_to_a_peer() {
    let handler = RequestHandler::new();
    let alpha = session_name("display-stale-alpha");
    let beta = session_name("display-stale-beta");
    for session_name in [&alpha, &beta] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 20, rows: 4 }),
                    environment: None,
                }))
                .await,
        );
    }

    let requester_pid = 43_101;
    let (requester_tx, _requester_rx) = mpsc::unbounded_channel();
    let requester_attach_id = handler
        .register_attach(requester_pid, alpha, requester_tx)
        .await;
    let (peer_tx, mut peer_rx) = mpsc::unbounded_channel();
    handler.register_attach(43_102, beta, peer_tx).await;
    while peer_rx.try_recv().is_ok() {}

    let pause = super::client_support::install_managed_client_resolution_pause(requester_pid);
    let display_handler = handler.clone();
    let display = tokio::spawn(async move {
        display_handler
            .dispatch(
                requester_pid,
                Request::DisplayMessageExt(Box::new(DisplayMessageExtRequest {
                    target: None,
                    print: false,
                    message: Some("must not reach peer".to_owned()),
                    target_client: Some("=".to_owned()),
                    empty_target_context: false,
                    duration_ms: None,
                    ignore_input: false,
                })),
            )
            .await
            .response
    });

    timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("display-message resolves the original attached client");
    handler
        .finish_attach(requester_pid, requester_attach_id)
        .await;
    pause.release.notify_one();

    assert_eq!(
        display.await.expect("display-message task joins"),
        Response::DisplayMessage(rmux_proto::DisplayMessageResponse::no_output())
    );
    assert!(
        peer_rx.try_recv().is_err(),
        "a stale explicit client must not fall back to another attached client"
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 100, rows: 4 }),
                    environment: None,
                }))
                .await,
        );
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

    let activity_sequence = handler.next_client_activity_sequence();
    assert!(handler
        .active_attach
        .lock()
        .await
        .record_client_activity(alpha_pid, activity_sequence));
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 100, rows: 4 }),
                    environment: None,
                }))
                .await,
        );
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 100, rows: 4 }),
                    environment: None,
                }))
                .await,
        );
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 20, rows: 4 }),
                    environment: None,
                }))
                .await,
        );
    }
    let pane_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .and_then(|session| session.active_pane_id())
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha,
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );

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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name,
                    detached: true,
                    size: Some(TerminalSize { cols: 20, rows: 4 }),
                    environment: None,
                }))
                .await,
        );
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

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
async fn display_message_ignore_input_preserves_outer_terminal_responses() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let attach_pid = 46;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 20, rows: 4 }),
                    environment: None,
                }))
                .await,
        );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
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
            .set_declared_client_size(client_size);
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 60, rows: 12 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 60, rows: 12 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 40, rows: 8 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 40, rows: 8 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 40, rows: 8 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
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
