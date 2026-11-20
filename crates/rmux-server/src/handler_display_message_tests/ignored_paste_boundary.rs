//! A bracketed paste that opens while `display-message -N` ignores input and
//! closes only after the message has expired.
//!
//! What the pane receives depends on its own DECSET 2004 state: the post-expiry
//! remainder of the paste goes through the ordinary paste path, which wraps it
//! only for a pane that enabled bracketed paste. Each case therefore pins that
//! state on a silent pane instead of inheriting it from the login shell. An
//! interactive Bash prompt enables DECSET 2004 asynchronously, so a fixture
//! running the default shell measured `$SHELL` and the prompt's timing rather
//! than the paste boundary.
//!
//! tmux 3.7b, measured on the same chunks through an attached client, drops
//! the ignored opener and forwards the late closer on its own: the pane
//! receives `AFTER` when it is unaware and `ESC[201~AFTER` when it is
//! bracketed. RMUX keeps the boundary balanced instead, so a bracketed pane
//! never sees a closer without its opener.

use super::*;
use rmux_core::input::mode::MODE_BRACKETPASTE;
use rmux_proto::NewSessionExtRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanePasteMode {
    Unaware,
    Bracketed,
}

impl PanePasteMode {
    /// Bytes the pane receives once the late closer and `AFTER` arrive. The
    /// ignored `BEFORE` body never reaches the pane in either mode.
    fn expected_pane_input(self) -> &'static [u8] {
        match self {
            Self::Unaware => b"AFTER",
            Self::Bracketed => b"\x1b[200~\x1b[201~AFTER",
        }
    }
}

#[cfg(unix)]
fn silent_pane_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn silent_pane_command() -> Vec<String> {
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

/// Creates an attached session whose only pane is silent, sets the pane's
/// bracketed-paste mode explicitly and starts capturing its input.
async fn attached_silent_pane(
    handler: &RequestHandler,
    session: &SessionName,
    attach_pid: u32,
    paste_mode: PanePasteMode,
) -> (PaneTarget, mpsc::UnboundedReceiver<AttachControl>) {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
                session_name: Some(session.clone()),
                working_directory: None,
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: Some(silent_pane_command()),
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            })))
            .await,
    );
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    let target = PaneTarget::new(session.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    let mut state = handler.state.lock().await;
    if paste_mode == PanePasteMode::Bracketed {
        state
            .append_bytes_to_pane_transcript_for_test(session, 0, 0, b"\x1b[?2004h")
            .expect("pane enables bracketed paste mode");
    }
    let pane_id = state
        .sessions
        .session(session)
        .and_then(|session| session.pane_id_in_window(0, 0))
        .expect("pane identity exists");
    let bracketed = state
        .pane_screen_state(session, pane_id)
        .is_some_and(|screen| screen.mode & MODE_BRACKETPASTE != 0);
    assert_eq!(
        bracketed,
        paste_mode == PanePasteMode::Bracketed,
        "the pane's DECSET 2004 state is pinned by the fixture"
    );
    state.start_pane_input_capture_for_test(&target);
    drop(state);
    (target, control_rx)
}

async fn assert_pane_input(
    handler: &RequestHandler,
    target: &PaneTarget,
    paste_mode: PanePasteMode,
) {
    assert_eq!(
        handler
            .attached_input_capture_for_test(target)
            .await
            .expect("pane input capture remains active"),
        paste_mode.expected_pane_input()
    );
}

async fn expiry_preserves_an_open_bracketed_paste_boundary(
    session: &str,
    attach_pid: u32,
    paste_mode: PanePasteMode,
) {
    let handler = RequestHandler::new();
    let alpha = session_name(session);
    let (target, mut control_rx) =
        attached_silent_pane(&handler, &alpha, attach_pid, paste_mode).await;
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
    assert_pane_input(&handler, &target, paste_mode).await;
}

#[tokio::test]
async fn display_message_expiry_preserves_an_open_paste_boundary_for_an_unaware_pane() {
    expiry_preserves_an_open_bracketed_paste_boundary(
        "message-paste-expiry",
        45_001,
        PanePasteMode::Unaware,
    )
    .await;
}

#[tokio::test]
async fn display_message_expiry_preserves_an_open_paste_boundary_for_a_bracketed_pane() {
    expiry_preserves_an_open_bracketed_paste_boundary(
        "message-paste-expiry-bracketed",
        45_011,
        PanePasteMode::Bracketed,
    )
    .await;
}

/// Expires the client's current message deterministically, as its timer would.
async fn expire_active_message(handler: &RequestHandler, attach_pid: u32) {
    let (identity, overlay_generation) = {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&attach_pid];
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
}

async fn rearmed_message_preserves_an_expired_open_paste_boundary(
    session: &str,
    attach_pid: u32,
    paste_mode: PanePasteMode,
) {
    let handler = RequestHandler::new();
    let alpha = session_name(session);
    let (target, mut control_rx) =
        attached_silent_pane(&handler, &alpha, attach_pid, paste_mode).await;

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
            expire_active_message(&handler, attach_pid).await;
        }
    }

    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"BODY-DURING-REPLACEMENT\n")
        .await
        .expect("replacement message keeps the inherited paste open");
    expire_active_message(&handler, attach_pid).await;
    handler
        .handle_attached_live_input(attach_pid, &mut pending, b"\x1b[201~AFTER")
        .await
        .expect("post-expiry closer preserves the paste boundary");
    assert!(pending.is_empty());
    assert_pane_input(&handler, &target, paste_mode).await;
}

#[tokio::test]
async fn rearmed_display_message_preserves_an_expired_open_paste_boundary_for_an_unaware_pane() {
    rearmed_message_preserves_an_expired_open_paste_boundary(
        "message-paste-expiry-rearm",
        45_002,
        PanePasteMode::Unaware,
    )
    .await;
}

#[tokio::test]
async fn rearmed_display_message_preserves_an_expired_open_paste_boundary_for_a_bracketed_pane() {
    rearmed_message_preserves_an_expired_open_paste_boundary(
        "message-paste-expiry-rearm-bracketed",
        45_012,
        PanePasteMode::Bracketed,
    )
    .await;
}
