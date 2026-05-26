use crate::handler::RequestHandler;
use crate::input_keys::MouseForwardEvent;
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use crate::server_access::current_owner_uid;
use rmux_core::{input::InputParser, Screen};
use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExtRequest, NewSessionExtRequest, SplitWindowExtRequest,
    SwitchClientExt2Request,
};
use rmux_proto::{
    AttachSessionResponse, AttachedKeystroke, CapturePaneRequest, CopyModeRequest,
    DetachClientRequest, ErrorResponse, KeyDispatched, KillSessionRequest, LayoutName,
    ListPanesRequest, ListWindowsRequest, NewSessionRequest, NewWindowRequest, OptionName,
    PaneTarget, RenameSessionRequest, Request, ResizePaneAdjustment, Response, RmuxError,
    ScopeSelector, SelectLayoutRequest, SelectLayoutTarget, SelectPaneRequest, SendKeysRequest,
    SessionName, SetOptionMode, SetOptionRequest, SplitWindowRequest, SplitWindowTarget,
    SwitchClientRequest, TerminalSize, WindowTarget, DEFAULT_MAX_FRAME_LENGTH,
};
#[cfg(unix)]
use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::sleep;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[cfg(unix)]
fn default_shell_window_name() -> String {
    "bash".to_owned()
}

#[cfg(windows)]
fn default_shell_window_name() -> String {
    if windows_command_exists("pwsh.exe") {
        return "pwsh.exe".to_owned();
    }
    if windows_powershell_path().is_some_and(|path| path.is_file()) {
        return "powershell.exe".to_owned();
    }
    std::env::var_os("COMSPEC")
        .and_then(|shell| Path::new(&shell).file_name().map(|name| name.to_owned()))
        .map(|name| name.to_string_lossy().trim_start_matches('-').to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "cmd.exe".to_owned())
}

#[cfg(windows)]
fn windows_command_exists(command: &str) -> bool {
    let Some(path_value) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_value).any(|directory| {
        let candidate = directory.join(command);
        candidate.is_file() && windows_shell_candidate_is_usable(&candidate)
    })
}

#[cfg(windows)]
fn windows_shell_candidate_is_usable(path: &Path) -> bool {
    !path
        .components()
        .any(|component| component.as_os_str().eq_ignore_ascii_case("WindowsApps"))
}

#[cfg(windows)]
fn windows_powershell_path() -> Option<std::path::PathBuf> {
    std::env::var_os("SystemRoot").map(|root| {
        std::path::PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    })
}

fn default_shell_pane_status() -> String {
    format!("{}|0|\n", default_shell_window_name())
}

fn take_render_frame(control: AttachControl) -> String {
    match control {
        AttachControl::Switch(target) => {
            String::from_utf8(target.render_frame).expect("render frame must be utf-8")
        }
        AttachControl::Detach => panic!("expected a switch refresh"),
        AttachControl::Exited => panic!("expected a switch refresh"),
        AttachControl::DetachKill => panic!("expected a switch refresh"),
        AttachControl::DetachExecShellCommand(_) => panic!("expected a switch refresh"),
        AttachControl::Overlay(_) => panic!("expected a switch refresh"),
        AttachControl::Write(_) => panic!("expected a switch refresh"),
        AttachControl::LockShellCommand(_) => panic!("expected a switch refresh"),
        AttachControl::AdvancePersistentOverlayState(_) => panic!("expected a switch refresh"),
        AttachControl::Suspend => panic!("expected a switch refresh"),
    }
}

fn take_switch_target(control: AttachControl) -> crate::pane_io::AttachTarget {
    match control {
        AttachControl::Switch(target) => *target,
        other => panic!("expected a switch refresh, got {other:?}"),
    }
}

/// Should this mount default to passthrough sessions?
///
/// The outer module that mounts this file via `#[path]` declares
/// `pub(super) const FORCE_PASSTHROUGH: bool = ...;`.  Two mounts —
/// one with `false`, one with `true` — let every test body run in
/// both modes without duplicating source.  Additionally,
/// `RMUX_TEST_FORCE_PASSTHROUGH=1` forces passthrough at runtime, so
/// individual `cargo test` invocations can flip the world too.
fn force_passthrough() -> bool {
    super::FORCE_PASSTHROUGH || std::env::var_os("RMUX_TEST_FORCE_PASSTHROUGH").is_some()
}

/// Early-return guard for tests that exercise features passthrough
/// sessions explicitly don't support (window splits, layout cycling,
/// chrome overlays with parsable render bytes).  Prints a notice so
/// the skip is visible in test output.
///
/// Use at the top of a test body:
/// ```ignore
/// if skip_if_passthrough("uses window splits") { return; }
/// ```
fn skip_if_passthrough(reason: &str) -> bool {
    if force_passthrough() {
        eprintln!("[passthrough mount] skipping: {reason}");
        true
    } else {
        false
    }
}

async fn create_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    #[cfg(unix)]
    set_unix_test_shell(handler, session).await;

    if force_passthrough() {
        let response = handler
            .handle(Request::NewSessionExt(NewSessionExtRequest {
                session_name: Some(session.clone()),
                working_directory: None,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                passthrough: true,
            }))
            .await;
        assert!(
            matches!(response, Response::NewSession(_)),
            "passthrough session should be created, got {response:?}"
        );
    } else {
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(
            matches!(response, Response::NewSession(_)),
            "normal session should be created, got {response:?}"
        );
    }
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    control_rx
}

#[cfg(unix)]
async fn set_unix_test_shell(handler: &RequestHandler, _session: &SessionName) {
    let mut state = handler.state.lock().await;
    state
        .options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            crate::test_shell::test_bash_path(),
            SetOptionMode::Replace,
        )
        .expect("test default-shell is valid");
}

async fn create_quiet_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    create_quiet_session(handler, session).await;
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    control_rx
}

#[cfg(unix)]
async fn create_quiet_session(handler: &RequestHandler, session: &SessionName) {
    create_session_with_command(handler, session, quiet_attached_command()).await;
}

#[cfg(windows)]
async fn create_quiet_session(handler: &RequestHandler, session: &SessionName) {
    let marker = format!("RMUX_QUIET_READY_{}", std::process::id());
    create_session_with_command(handler, session, quiet_ready_command(&marker)).await;
    let target = PaneTarget::new(session.clone(), 0);
    wait_for_capture_containing(
        handler,
        target.clone(),
        &marker,
        "quiet Windows attach fixture should reach a stable shell frame",
    )
    .await;
    replace_transcript_contents(handler, &target, TerminalSize { cols: 80, rows: 24 }, b"").await;
}

async fn create_session_with_command(
    handler: &RequestHandler,
    session: &SessionName,
    command: Vec<String>,
) {
    let response = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(command),
            process_command: None,
            passthrough: force_passthrough(),
        }))
        .await;
    assert!(
        matches!(response, Response::NewSession(_)),
        "quiet test session should be created, got {response:?}"
    );
}

#[cfg(windows)]
fn quiet_ready_command(marker: &str) -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        format!("echo {marker} & ping -n 120 127.0.0.1 >NUL"),
    ]
}

#[cfg(unix)]
fn quiet_ready_command(marker: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!("printf '{marker}\\n'; sleep 60"),
    ]
}

#[cfg(unix)]
fn quiet_attached_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

async fn active_panes(handler: &RequestHandler, session: &SessionName) -> String {
    let response = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: session.clone(),
            format: Some("#{pane_index}:#{pane_active}".to_owned()),
            target_window_index: None,
        }))
        .await;
    let Response::ListPanes(response) = response else {
        panic!("expected list-panes response, got {response:?}");
    };
    String::from_utf8(response.output.stdout().to_vec()).expect("list-panes stdout is utf-8")
}

async fn pane_terminal_size(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) -> TerminalSize {
    let master = {
        let mut state = handler.state.lock().await;
        state
            .clone_pane_master_if_alive(session_name, window_index, pane_index)
            .expect("pane terminal is alive")
    };
    let winsize = master.size().expect("pane winsize available");
    TerminalSize {
        cols: winsize.cols,
        rows: winsize.rows,
    }
}

async fn active_windows(handler: &RequestHandler, session: &SessionName) -> String {
    let response = handler
        .handle(Request::ListWindows(ListWindowsRequest {
            target: session.clone(),
            format: Some("#{window_index}:#{window_active}".to_owned()),
        }))
        .await;
    let Response::ListWindows(response) = response else {
        panic!("expected list-windows response, got {response:?}");
    };
    String::from_utf8(response.output.stdout().to_vec()).expect("list-windows stdout is utf-8")
}

async fn current_layout(handler: &RequestHandler, session: &SessionName) -> LayoutName {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window()
        .layout()
}

async fn select_layout(handler: &RequestHandler, session: &SessionName, layout: LayoutName) {
    assert!(matches!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::new(session.clone())),
                layout,
            }))
            .await,
        Response::SelectLayout(_)
    ));
}

async fn pane_mode_status(handler: &RequestHandler, session: &SessionName) -> String {
    let response = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: session.clone(),
            format: Some(
                "#{pane_in_mode}:#{pane_mode}:#{search_present}:#{selection_present}".to_owned(),
            ),
            target_window_index: None,
        }))
        .await;
    let Response::ListPanes(response) = response else {
        panic!("expected list-panes response, got {response:?}");
    };
    String::from_utf8(response.output.stdout().to_vec()).expect("list-panes stdout is utf-8")
}

async fn display_target_format(
    handler: &RequestHandler,
    target: PaneTarget,
    format: &str,
) -> String {
    let response = handler
        .handle(Request::DisplayMessage(rmux_proto::DisplayMessageRequest {
            target: Some(rmux_proto::Target::Pane(target)),
            print: true,
            message: Some(format.to_owned()),
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    String::from_utf8(output.stdout().to_vec()).expect("display-message stdout is utf-8")
}

fn drain_attach_controls(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    while control_rx.try_recv().is_ok() {}
}

fn oversized_unterminated_sgr_mouse_input() -> Vec<u8> {
    let mut bytes = b"\x1b[<".to_vec();
    bytes.resize(DEFAULT_MAX_FRAME_LENGTH + 1, b'1');
    bytes
}

fn assert_partial_control_bound<T>(result: std::io::Result<T>, context: &str) {
    let error = match result {
        Ok(_) => panic!("partial control input should be rejected after the bound"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    let message = error.to_string();
    assert!(
        message.contains(context),
        "error should name {context:?}, got {message:?}"
    );
    assert!(
        message.contains("maximum"),
        "error should include the retained byte limit, got {message:?}"
    );
}

async fn recv_overlay_frame(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
) -> String {
    let overlay = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let AttachControl::Overlay(overlay) = control_rx.recv().await.expect(context) {
                break overlay;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for overlay: {context}"));
    String::from_utf8_lossy(&overlay.frame).into_owned()
}

async fn capture_pane_print(handler: &RequestHandler, target: PaneTarget) -> String {
    let response = handler
        .handle(Request::CapturePane(CapturePaneRequest {
            target,
            start: None,
            end: None,
            print: true,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        }))
        .await;
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane response, got {response:?}");
    };
    let output = response
        .output
        .expect("capture-pane -p should return command output");
    String::from_utf8(output.stdout().to_vec()).expect("capture-pane stdout is utf-8")
}

async fn wait_for_capture_containing(
    handler: &RequestHandler,
    target: PaneTarget,
    needle: &str,
    context: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let capture = capture_pane_print(handler, target.clone()).await;
        if capture.contains(needle) {
            return capture;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}, got {capture:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

async fn prepare_attached_shell_prompt(handler: &RequestHandler, target: &PaneTarget) {
    let [set_prompt, clear_screen] = attached_shell_prompt_commands();
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec![set_prompt, "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SendKeys(SendKeysRequest {
                target: target.clone(),
                keys: vec![clear_screen, "Enter".to_owned()],
            }))
            .await,
        Response::SendKeys(_)
    ));
    wait_for_capture_containing(
        handler,
        target.clone(),
        "PROMPT>",
        "attached shell prompt must be ready",
    )
    .await;
}

#[cfg(unix)]
fn attached_shell_prompt_commands() -> [String; 2] {
    ["export PS1='PROMPT> '", "clear"].map(str::to_owned)
}

#[cfg(windows)]
fn attached_shell_prompt_commands() -> [String; 2] {
    ["function global:prompt { 'PROMPT> ' }", "Clear-Host"].map(str::to_owned)
}

async fn wait_for_dead_pane(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let exited = {
            let mut state = handler.state.lock().await;
            state
                .clone_pane_master_if_alive(session_name, window_index, pane_index)
                .is_err()
        };
        if exited {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pane {session_name}:{window_index}.{pane_index} to exit"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_session_removed(handler: &RequestHandler, session_name: &SessionName) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let exists = {
            let state = handler.state.lock().await;
            state.sessions.session(session_name).is_some()
        };
        if !exists {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for session {session_name} to be removed"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

use crate::handler::input_capture::RawPaneInputProbe;

async fn replace_transcript_contents(
    handler: &RequestHandler,
    target: &PaneTarget,
    size: TerminalSize,
    content: &[u8],
) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("session transcript must exist")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .history_limit();
    let mut screen = Screen::new(size, history_limit);
    let mut parser = InputParser::new();
    parser.parse(content, &mut screen);
    transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .set_screen_for_test(screen);
}

#[path = "handler_attach_tests/lifecycle.rs"]
mod lifecycle;

#[path = "handler_attach_tests/prefix_navigation.rs"]
mod prefix_navigation;

#[path = "handler_attach_tests/display_panes.rs"]
mod display_panes;

#[path = "handler_attach_tests/copy_mode_keys.rs"]
mod copy_mode_keys;

#[path = "handler_attach_tests/copy_mode_render.rs"]
mod copy_mode_render;

#[path = "handler_attach_tests/copy_mode_motion.rs"]
mod copy_mode_motion;

#[path = "handler_attach_tests/copy_mode_search.rs"]
mod copy_mode_search;

#[path = "handler_attach_tests/copy_mode_selection_yank.rs"]
mod copy_mode_selection_yank;

#[path = "handler_attach_tests/mode_tree_clock.rs"]
mod mode_tree_clock;

#[path = "handler_attach_tests/attach_mutations.rs"]
mod attach_mutations;

#[path = "handler_attach_tests/attach_render.rs"]
mod attach_render;

#[path = "handler_attach_tests/attached_prefix_lifecycle.rs"]
mod attached_prefix_lifecycle;

#[path = "handler_attach_tests/multi_client.rs"]
mod multi_client;

#[path = "handler_attach_tests/server_lifecycle.rs"]
mod server_lifecycle;

#[path = "handler_attach_tests/client_security.rs"]
mod client_security;

#[path = "handler_attach_tests/passthrough_input.rs"]
mod passthrough_input;
