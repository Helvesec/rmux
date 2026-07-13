use super::attach_support::AttachRegistration;
use super::RequestHandler;
use crate::input_keys::{MouseForwardEvent, MAX_SGR_MOUSE_FRAME_BYTES};
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use crate::server_access::current_owner_uid;
use rmux_core::{input::InputParser, Screen};
use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
    NewSessionExtRequest, SplitWindowExtRequest, SwitchClientExt2Request,
};
use rmux_proto::{
    AttachSessionResponse, AttachedKeystroke, CapturePaneRequest, CopyModeRequest,
    DetachClientExtRequest, DetachClientRequest, ErrorResponse, KeyDispatched, KillSessionRequest,
    LayoutName, LinkWindowRequest, ListPanesRequest, ListWindowsRequest, NewSessionRequest,
    NewWindowRequest, OptionName, PaneTarget, RenameSessionRequest, Request, ResizePaneAdjustment,
    Response, RmuxError, ScopeSelector, SelectLayoutRequest, SelectLayoutTarget, SelectPaneRequest,
    SelectWindowRequest, SendKeysRequest, SessionName, SetOptionMode, SetOptionRequest,
    SplitWindowRequest, SplitWindowTarget, SwitchClientRequest, TerminalSize, WindowTarget,
    CAPABILITY_ATTACH_RENDER,
};
#[cfg(unix)]
use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::sleep;

#[cfg(windows)]
const ATTACH_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(not(windows))]
const ATTACH_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

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
        AttachControl::InteractiveInput => panic!("expected a switch refresh"),
        AttachControl::Refresh => panic!("expected a switch refresh"),
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

async fn recv_attach_control(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
) -> AttachControl {
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, control_rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for attach control: {context}"))
        .unwrap_or_else(|| panic!("attach control channel closed while waiting for {context}"))
}

async fn recv_render_frame(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
) -> String {
    take_render_frame(recv_attach_control(control_rx, context).await)
}

async fn recv_switch_target(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
) -> crate::pane_io::AttachTarget {
    take_switch_target(recv_attach_control(control_rx, context).await)
}

async fn recv_matching_attach_control(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
    matches: impl Fn(&AttachControl) -> bool,
) -> AttachControl {
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, async {
        while let Some(control) = control_rx.recv().await {
            if matches(&control) {
                return control;
            }
        }
        panic!("attach control channel closed while waiting for {context}");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for attach control: {context}"))
}

async fn create_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    #[cfg(windows)]
    create_quiet_session(handler, session).await;
    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        set_unix_test_shell(handler, session).await;

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
    }
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    control_rx
}

#[tokio::test]
async fn web_render_refreshes_are_marked_pending_before_building_switches() {
    let handler = RequestHandler::new();
    let session = session_name("web-refresh-coalesce");
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let uid = current_owner_uid();
    handler
        .register_attach_with_access(
            77,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::default(),
                flags: super::attach_support::ClientFlags::default(),
                render_stream: true,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
        )
        .await
        .expect("attach registration succeeds");

    handler.refresh_attached_session(&session).await;
    handler.refresh_attached_session(&session).await;

    assert!(matches!(control_rx.try_recv(), Ok(AttachControl::Refresh)));
    assert!(matches!(control_rx.try_recv(), Err(TryRecvError::Empty)));
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach.by_pid.get(&77).expect("attach is active");
    assert!(active.render_refresh_pending);
}

#[tokio::test]
async fn refresh_attached_session_removes_clients_over_backlog_limit() {
    let handler = RequestHandler::new();
    let session = session_name("refresh-backlog");
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let control_backlog = Arc::new(AtomicUsize::new(
        super::attach_support::ATTACH_CONTROL_BACKLOG_LIMIT,
    ));
    let closing = Arc::new(AtomicBool::new(false));
    let uid = current_owner_uid();
    handler
        .register_attach_with_access(
            77,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: control_backlog.clone(),
                closing: closing.clone(),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::default(),
                flags: super::attach_support::ClientFlags::default(),
                render_stream: false,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
        )
        .await
        .expect("attach registration succeeds");

    handler.refresh_attached_session(&session).await;

    assert!(closing.load(Ordering::SeqCst));
    assert!(!handler.active_attach.lock().await.by_pid.contains_key(&77));
    assert!(matches!(control_rx.try_recv(), Ok(AttachControl::Detach)));
    assert_eq!(
        control_backlog.load(Ordering::Acquire),
        super::attach_support::ATTACH_CONTROL_BACKLOG_LIMIT,
        "refresh should not enqueue another tracked frame once the backlog is saturated"
    );
}

#[cfg(any(unix, windows))]
async fn create_line_exiting_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let marker = format!("RMUX_LINE_EXIT_READY_{}", std::process::id());
    create_session_with_command(handler, session, line_exiting_command(&marker)).await;
    let target = PaneTarget::new(session.clone(), 0);
    wait_for_capture_containing(
        handler,
        target.clone(),
        &marker,
        "Windows attached-exit fixture should reach its input loop",
    )
    .await;
    replace_transcript_contents(handler, &target, TerminalSize { cols: 80, rows: 24 }, b"").await;

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    control_rx
}

#[cfg(windows)]
async fn create_line_echo_attached_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let marker = format!("RMUX_LINE_ECHO_READY_{}", std::process::id());
    create_session_with_command(handler, session, line_echo_command(&marker)).await;
    let target = PaneTarget::new(session.clone(), 0);
    wait_for_capture_containing(
        handler,
        target.clone(),
        &marker,
        "Windows attached UTF-8 fixture should reach its input loop",
    )
    .await;
    replace_transcript_contents(handler, &target, TerminalSize { cols: 80, rows: 24 }, b"").await;

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
            "/bin/bash".to_owned(),
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
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
            client_environment: None,
            skip_environment_update: false,
        })))
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
fn line_exiting_command(marker: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!(
            "printf '%s\\n' '{marker}'; \
             while IFS= read -r line; do \
                 if [ \"$line\" = exit ] || [ \"$line\" = RMUX_EXIT ]; then \
                     printf 'logout\\n'; \
                     exit 0; \
                 fi; \
             done"
        ),
    ]
}

#[cfg(windows)]
fn line_exiting_command(marker: &str) -> Vec<String> {
    windows_cmd_command(format!(
        "echo {marker} & set \"line=\" & set /p \"line=\" & if /I \"!line!\"==\"RMUX_EXIT\" exit /b 0 & ping -n 120 127.0.0.1 >NUL"
    ))
}

#[cfg(windows)]
fn line_echo_command(marker: &str) -> Vec<String> {
    windows_cmd_command(format!(
        "chcp 65001 >NUL & echo {marker} & set \"line=\" & set /p \"line=\" & echo ECHO:!line! & ping -n 60 127.0.0.1 >NUL"
    ))
}

#[cfg(windows)]
fn windows_cmd_command(command: String) -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/v:on".to_owned(),
        "/c".to_owned(),
        command,
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

#[cfg(windows)]
fn quiet_attached_command() -> Vec<String> {
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
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn active_panes(handler: &RequestHandler, session: &SessionName) -> String {
    let response = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: session.clone(),
            format: Some("#{pane_index}:#{pane_active}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
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
        .handle(Request::ListWindows(Box::new(ListWindowsRequest {
            target: session.clone(),
            format: Some("#{window_index}:#{window_active}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
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
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: session.clone(),
            format: Some(
                "#{pane_in_mode}:#{pane_mode}:#{search_present}:#{selection_present}".to_owned(),
            ),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
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
            empty_target_context: false,
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

fn bounded_unterminated_sgr_mouse_input() -> Vec<u8> {
    let mut bytes = b"\x1b[<".to_vec();
    bytes.resize(MAX_SGR_MOUSE_FRAME_BYTES, b'1');
    bytes
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
        .handle(Request::CapturePane(Box::new(CapturePaneRequest {
            target,
            start: None,
            end: None,
            print: true,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            include_format: false,
            hyperlinks: false,
            line_numbers: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        })))
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
        attached_shell_prompt_ready_needle(),
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
    [
        "Remove-Module PSReadLine -ErrorAction SilentlyContinue; function global:prompt { 'PROMPT> ' }; Write-Output ('RMUX_PROMPT_' + 'READY')",
        "$null = 0; Write-Output ('RMUX_PROMPT_' + 'CLEAR')",
    ]
    .map(str::to_owned)
}

#[cfg(unix)]
fn attached_shell_prompt_ready_needle() -> &'static str {
    "PROMPT>"
}

#[cfg(windows)]
fn attached_shell_prompt_ready_needle() -> &'static str {
    "RMUX_PROMPT_CLEAR\nPROMPT>"
}

async fn wait_for_dead_pane(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) {
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
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
    let deadline = tokio::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
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

use super::input_capture::RawPaneInputProbe;

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

#[path = "handler_attach_tests/cleanup_identity.rs"]
mod cleanup_identity;
#[path = "handler_attach_tests/multi_client.rs"]
mod multi_client;
#[path = "handler_attach_tests/resize_selection_race.rs"]
mod resize_selection_race;

#[path = "handler_attach_tests/server_lifecycle.rs"]
mod server_lifecycle;

#[path = "handler_attach_tests/client_security.rs"]
mod client_security;
