use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmux_core::command_parser::CommandParser;
use rmux_core::events::OutputCursorItem;
use rmux_core::{OptionStore, PaneGeometry, TerminalPassthrough};
use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachShellCommand,
    AttachedKeystroke, BindKeyRequest, KeyDispatched, KillSessionRequest, NewSessionRequest,
    OptionName, PaneTarget, Request, Response, ScopeSelector, SessionName, SetOptionMode,
    TerminalSize, WaitForMode, WaitForRequest,
};
use rmux_pty::PtyPair;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::attach_output_batch::{collect_attach_output_batch, AttachOutputBatch};
use super::attach_transport::AttachTransport;
use super::control::{
    apply_pending_attach_controls, coalesce_render_switches, PendingAttachAction,
    PendingAttachInputState,
};
use super::exit_log::AttachExitReason;
use super::pending_escape::PendingEscapeFlush;
use super::wire::open_attach_target;
use super::wire::recv_pane_output;
use super::{
    clear_close_pane_output_after_refresh_if_target_changed, consume_predicted_echo,
    finish_pending_attach_exit_with_batch, forward_attach, install_live_attach_input_apply_pause,
    install_live_attach_input_validation_pause, is_predictable_local_echo, pane_output_channel,
    pane_output_channel_with_limits, pending_attach_exit_output_batch,
    predictable_local_echo_prefix_len, process_attach_data_payload, process_socket_messages,
    should_emit_overlay, sync_pending_escape_flush_with_escape_time, AttachControl, AttachTarget,
    LiveAttachInputContext, OverlayFrame, PredictedEcho,
};
use crate::daemon::ShutdownHandle;
use crate::handler::RequestHandler;
use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
use crate::renderer::PaneRenderDeltaFrame;

mod persistent_overlay;

async fn dispatch_live_attach_data_for_test(
    live_input: LiveAttachInputContext,
    bytes: &[u8],
) -> std::io::Result<bool> {
    dispatch_live_attach_message_for_test(live_input, AttachMessage::Data(bytes.to_vec())).await
}

async fn dispatch_live_attach_message_for_test(
    live_input: LiveAttachInputContext,
    message: AttachMessage,
) -> std::io::Result<bool> {
    let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encode_attach_message(&message).expect("encode attach input"));
    let mut pending_input = Vec::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut active_emit_cache = None;
    let mut locked = false;
    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
}

#[tokio::test]
async fn forward_attach_resize_during_command_prompt_keeps_exact_identity_alive() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name =
        SessionName::new("resize-command-prompt-identity").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(attach_pid).await;

    let prompt = CommandParser::new()
        .parse_one_group("command-prompt -b -p resize")
        .expect("command-prompt parses");
    handler
        .execute_parsed_commands_for_test(attach_pid, prompt)
        .await
        .expect("background prompt starts");

    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&session_name, b"BASE", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        LiveAttachInputContext::new(Arc::clone(&handler), identity),
        false,
    ));
    let _ = read_attach_data_until(&mut peer, b"BASE").await;

    peer.write_all(
        &encode_attach_message(&AttachMessage::Resize(TerminalSize {
            cols: 100,
            rows: 30,
        }))
        .expect("encode resize"),
    )
    .await
    .expect("send resize");
    peer.write_all(
        &encode_attach_message(&AttachMessage::Keystroke(AttachedKeystroke::new(
            b"x".to_vec(),
        )))
        .expect("encode prompt key"),
    )
    .await
    .expect("send prompt key after resize");

    tokio::time::timeout(Duration::from_secs(2), async {
        let mut decoder = AttachFrameDecoder::new();
        let mut bytes = [0_u8; 4096];
        loop {
            let bytes_read = peer.read(&mut bytes).await.expect("read attach output");
            assert!(
                bytes_read > 0,
                "resize closed the attach before prompt input"
            );
            decoder.push_bytes(&bytes[..bytes_read]);
            while let Some(message) = decoder.next_message().expect("decode attach output") {
                if message == AttachMessage::KeyDispatched(KeyDispatched::new(1)) {
                    return;
                }
            }
        }
    })
    .await
    .expect("prompt key acknowledgement after resize");
    assert_eq!(
        handler.active_attach_identity(attach_pid).await,
        Some(identity),
        "resize must preserve the exact attach identity"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    attach_task
        .await
        .expect("attach task join")
        .expect("attach exits cleanly");
}

async fn create_attach_input_test_session(handler: &RequestHandler, name: &str) -> PaneTarget {
    let session_name = SessionName::new(name).expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler.start_attached_input_capture_for_test(&target).await;
    target
}

#[tokio::test]
async fn same_pid_replacement_publishes_while_validated_old_input_is_paused() {
    let attach_pid = 910_031;

    // A has passed the socket-level validation but has not yet applied its
    // input. B must still publish promptly: no input or command await may hold
    // registration hostage. Once resumed, A must fail closed at the mutation
    // boundary and must not route through either session.
    let handler = Arc::new(RequestHandler::new());
    let alpha = create_attach_input_test_session(&handler, "identity-order-alpha").await;
    let beta = create_attach_input_test_session(&handler, "identity-order-beta").await;
    let (alpha_control_tx, mut alpha_control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.session_name().clone(), alpha_control_tx)
        .await;
    let alpha_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    let pause = install_live_attach_input_apply_pause(alpha_input.identity);
    let input_task = tokio::spawn(dispatch_live_attach_data_for_test(alpha_input, b"A-WINS"));
    pause.reached.notified().await;

    let replacement_handler = Arc::clone(&handler);
    let beta_name = beta.session_name().clone();
    let (beta_control_tx, _beta_control_rx) = mpsc::unbounded_channel();
    let replacement_task = tokio::spawn(async move {
        replacement_handler
            .register_attach(attach_pid, beta_name, beta_control_tx)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), replacement_task)
        .await
        .expect("same-PID replacement must not wait for paused old input")
        .expect("replacement task join");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), alpha_control_rx.recv())
            .await
            .expect("old attach must receive Detach promptly"),
        Some(AttachControl::Detach)
    ));

    pause.release.notify_one();
    assert!(
        input_task.await.expect("input task join").is_err(),
        "A input must fail closed after B publishes"
    );
    assert_eq!(
        handler.attached_input_capture_for_test(&alpha).await,
        Some(Vec::new())
    );
    assert_eq!(
        handler.attached_input_capture_for_test(&beta).await,
        Some(Vec::new())
    );

    // B already owns the PID before A's next frame reaches the socket loop.
    // The early stale check must reject it too.
    let handler = Arc::new(RequestHandler::new());
    let alpha = create_attach_input_test_session(&handler, "identity-stale-alpha").await;
    let beta = create_attach_input_test_session(&handler, "identity-stale-beta").await;
    let (alpha_control_tx, _alpha_control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.session_name().clone(), alpha_control_tx)
        .await;
    let stale_alpha_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    let (beta_control_tx, _beta_control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, beta.session_name().clone(), beta_control_tx)
        .await;

    let stale = dispatch_live_attach_data_for_test(stale_alpha_input, b"MUST-NOT-ROUTE").await;
    assert!(
        stale.is_err(),
        "old same-PID socket input must fail closed once B is published"
    );
    assert_eq!(
        handler.attached_input_capture_for_test(&alpha).await,
        Some(Vec::new())
    );
    assert_eq!(
        handler.attached_input_capture_for_test(&beta).await,
        Some(Vec::new())
    );
}

#[tokio::test]
async fn same_pid_replacement_publishes_while_old_binding_waits() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let channel = "attach-identity-blocked-binding";
    let alpha = create_attach_input_test_session(&handler, "identity-wait-alpha").await;
    let beta = create_attach_input_test_session(&handler, "identity-wait-beta").await;
    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "identity-wait".to_owned(),
            key: "x".to_owned(),
            note: Some("identity-wait".to_owned()),
            repeat: false,
            command: Some(vec![format!("wait-for {channel} ; detach-client")]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)), "{bound:?}");

    let (alpha_control_tx, mut alpha_control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.session_name().clone(), alpha_control_tx)
        .await;
    handler
        .set_attached_key_table_for_test(attach_pid, Some("identity-wait".to_owned()))
        .await
        .expect("activate blocking test key table");
    while alpha_control_rx.try_recv().is_ok() {}
    let alpha_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    let input_task = tokio::spawn(dispatch_live_attach_message_for_test(
        alpha_input,
        AttachMessage::Keystroke(AttachedKeystroke::new(b"x".to_vec())),
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while handler.wait_for_counts(channel).0 != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("binding reaches wait-for before replacement");

    let (beta_control_tx, mut beta_control_rx) = mpsc::unbounded_channel();
    tokio::time::timeout(
        Duration::from_secs(2),
        handler.register_attach(attach_pid, beta.session_name().clone(), beta_control_tx),
    )
    .await
    .expect("replacement must not wait for the blocked binding");
    let beta_identity = handler.active_attach_identity_for_test(attach_pid).await;
    while beta_control_rx.try_recv().is_ok() {}
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match alpha_control_rx.recv().await {
                Some(AttachControl::Detach) => break,
                Some(_) => continue,
                None => panic!("old attach control channel closed before Detach"),
            }
        }
    })
    .await
    .expect("old attach receives Detach");

    let signaled = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(signaled, Response::WaitFor(_)), "{signaled:?}");
    let _old_input_result = tokio::time::timeout(Duration::from_secs(2), input_task)
        .await
        .expect("old binding unwinds after signal")
        .expect("input task join");
    assert!(
        handler.current_live_attach_input(beta_identity).await,
        "the old queued binding must not detach the same-PID replacement"
    );
    while let Ok(control) = beta_control_rx.try_recv() {
        assert!(
            !matches!(
                control,
                AttachControl::Detach
                    | AttachControl::DetachKill
                    | AttachControl::DetachExecShellCommand(_)
            ),
            "the old queued binding sent a detach control to its replacement"
        );
    }
    assert_eq!(
        handler.attached_input_capture_for_test(&alpha).await,
        Some(Vec::new())
    );
    assert_eq!(
        handler.attached_input_capture_for_test(&beta).await,
        Some(Vec::new())
    );
}

#[tokio::test]
async fn unlock_flushes_resume_output_before_following_blocking_keystroke() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let channel = "attach-unlock-output-barrier";
    let session_name = SessionName::new("unlock-output-barrier").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "unlock-output-barrier".to_owned(),
            key: "x".to_owned(),
            note: Some("unlock output barrier".to_owned()),
            repeat: false,
            command: Some(vec![format!("wait-for {channel}")]),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)), "{bound:?}");

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    handler
        .set_attached_key_table_for_test(attach_pid, Some("unlock-output-barrier".to_owned()))
        .await
        .expect("activate blocking test key table");

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let mut decoder = AttachFrameDecoder::new();
    let mut coalesced = encode_attach_message(&AttachMessage::Unlock).expect("encode unlock");
    coalesced.extend_from_slice(
        &encode_attach_message(&AttachMessage::Keystroke(AttachedKeystroke::new(
            b"x".to_vec(),
        )))
        .expect("encode blocking keystroke"),
    );
    decoder.push_bytes(&coalesced);
    let live_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    let mut current_target =
        open_attach_target(test_attach_target(&session_name, b"RESUMED", None), false)
            .expect("open attach target");

    let input_task = tokio::spawn(async move {
        let mut pending_input = Vec::new();
        let mut pending_escape_flush = PendingEscapeFlush::default();
        let mut active_emit_cache = None;
        let mut locked = true;
        process_socket_messages(
            &mut decoder,
            &stream,
            &live_input,
            Some(&mut current_target),
            PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
            &mut active_emit_cache,
            &mut locked,
        )
        .await?;
        process_socket_messages(
            &mut decoder,
            &stream,
            &live_input,
            Some(&mut current_target),
            PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
            &mut active_emit_cache,
            &mut locked,
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while handler.wait_for_counts(channel).0 != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("following binding reaches wait-for");
    assert!(
        !input_task.is_finished(),
        "the following keystroke must still be waiting"
    );
    let resume = read_attach_data_until(&mut peer, b"RESUMED").await;
    assert!(
        resume
            .windows(b"RESUMED".len())
            .any(|bytes| bytes == b"RESUMED"),
        "unlock must restore the terminal before the following binding completes"
    );

    let signaled = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(signaled, Response::WaitFor(_)), "{signaled:?}");
    input_task
        .await
        .expect("input task join")
        .expect("coalesced input processing succeeds");
}

#[test]
fn pending_escape_wrapper_covers_apc_csi_paste_and_excludes_utf8() {
    let mut flush = PendingEscapeFlush::default();
    let escape_time = Duration::from_millis(5);
    let before_apc = Instant::now();

    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b_Gi=7;payload", escape_time);
    assert!(
        flush
            .deadline()
            .is_some_and(|deadline| deadline > before_apc + Duration::from_secs(1)),
        "the production wrapper must give Kitty APC a stream idle budget, not escape-time"
    );

    flush.clear();
    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b[12", escape_time);
    assert!(
        flush.deadline().is_some(),
        "numeric CSI retention must stay timed"
    );

    flush.clear();
    let before_paste = Instant::now();
    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b[200~body", escape_time);
    assert!(
        flush
            .deadline()
            .is_some_and(|deadline| deadline > before_paste + Duration::from_secs(1)),
        "streaming bracketed paste must use the long stream idle budget"
    );

    sync_pending_escape_flush_with_escape_time(&mut flush, b"\xe6\x97", escape_time);
    assert!(
        flush.deadline().is_none(),
        "partial UTF-8 must never inherit the escape deadline"
    );
}

#[test]
fn pending_escape_wrapper_resets_stream_deadline_for_new_keyboard_suffix() {
    let mut flush = PendingEscapeFlush::default();
    let escape_time = Duration::from_millis(5);

    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b_Gpayload", escape_time);
    let stream_deadline = flush.deadline().expect("APC stream arms");
    let before_escape = Instant::now();
    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b", escape_time);
    let escape_deadline = flush.deadline().expect("post-stream Escape arms");

    assert!(escape_deadline >= before_escape + escape_time);
    assert!(
        escape_deadline < stream_deadline,
        "a consumed stream followed by Escape must not inherit its long idle deadline"
    );
    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b[12", Duration::from_secs(1));
    assert_eq!(
        flush.deadline(),
        Some(escape_deadline),
        "numeric CSI growth keeps the first keyboard ambiguity deadline"
    );
}

#[test]
fn pending_escape_wrapper_times_only_unterminated_overlong_mouse_input() {
    let mut flush = PendingEscapeFlush::default();
    let escape_time = Duration::from_millis(5);

    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b[<700000", escape_time);
    assert!(
        flush.deadline().is_some(),
        "an unterminated overflowing decimal remains bounded by escape-time"
    );

    sync_pending_escape_flush_with_escape_time(&mut flush, b"\x1b[<700000;1;1M", escape_time);
    assert!(
        flush.deadline().is_none(),
        "a lexically complete invalid mouse frame must leave the retained-input grammar"
    );
}

async fn pending_escape_socket_fixture(
    session: &str,
) -> (
    LiveAttachInputContext,
    AttachTransport,
    tokio::net::UnixStream,
    mpsc::UnboundedReceiver<AttachControl>,
) {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new(session).expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name, control_tx)
        .await;
    handler.start_attached_input_capture_for_test(&target).await;
    let (stream, peer) = tokio::net::UnixStream::pair().expect("attach stream pair");

    (
        LiveAttachInputContext::current_for_test(handler, attach_pid).await,
        AttachTransport::from(stream),
        peer,
        control_rx,
    )
}

struct PendingEscapeSchedulerFixture {
    handler: Arc<RequestHandler>,
    target: PaneTarget,
    peer: tokio::net::UnixStream,
    shutdown: watch::Sender<()>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn current_attach_target(
    handler: &RequestHandler,
    attach_pid: u32,
    session_name: &SessionName,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> AttachTarget {
    handler
        .refresh_attached_client(attach_pid, session_name)
        .await;
    let target = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Switch(target)) => break *target,
                Some(_) => continue,
                None => panic!("attach control channel closed before initial target"),
            }
        }
    })
    .await
    .expect("timed out building the initial attach target");
    handler
        .clear_attached_render_refresh_pending(attach_pid)
        .await;
    target
}

impl PendingEscapeSchedulerFixture {
    async fn start(session: &str) -> Self {
        let handler = Arc::new(RequestHandler::new());
        let attach_pid = std::process::id();
        let session_name = SessionName::new(session).expect("valid session name");
        let target = PaneTarget::with_window(session_name.clone(), 0, 0);
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)), "{created:?}");
        let escape_time = handler
            .handle(Request::SetOption(rmux_proto::SetOptionRequest {
                scope: rmux_proto::ScopeSelector::Global,
                option: rmux_proto::OptionName::EscapeTime,
                value: "500".to_owned(),
                mode: rmux_proto::SetOptionMode::Replace,
            }))
            .await;
        assert!(
            matches!(escape_time, Response::SetOption(_)),
            "{escape_time:?}"
        );

        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, session_name.clone(), control_tx)
            .await;
        handler.start_attached_input_capture_for_test(&target).await;
        let initial_target =
            current_attach_target(&handler, attach_pid, &session_name, &mut control_rx).await;
        let (shutdown, shutdown_rx) = watch::channel(());
        let (stream, peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
        let task = tokio::spawn(forward_attach(
            stream,
            initial_target,
            Vec::new(),
            shutdown_rx,
            control_rx,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await,
            false,
        ));

        Self {
            handler,
            target,
            peer,
            shutdown,
            task,
        }
    }

    async fn send(&mut self, message: AttachMessage) {
        self.send_batch(&[message]).await;
    }

    async fn send_batch(&mut self, messages: &[AttachMessage]) {
        let mut encoded = Vec::new();
        for message in messages {
            encoded
                .extend_from_slice(&encode_attach_message(message).expect("encode attach input"));
        }
        self.peer
            .write_all(&encoded)
            .await
            .expect("write attach input");
    }

    async fn wait_for_capture(&self, matches: impl Fn(&[u8]) -> bool, label: &str) -> Vec<u8> {
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let captured = self
                    .handler
                    .attached_input_capture_for_test(&self.target)
                    .await
                    .expect("input capture remains installed");
                if matches(&captured) {
                    break captured;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        match result {
            Ok(captured) => captured,
            Err(_) => {
                let captured = self
                    .handler
                    .attached_input_capture_for_test(&self.target)
                    .await;
                panic!(
                    "timed out waiting for {label}; capture={captured:?}, attach_finished={}",
                    self.task.is_finished()
                );
            }
        }
    }

    async fn finish(self) {
        self.shutdown.send(()).expect("request attach shutdown");
        assert!(self.task.await.expect("attach task join").is_ok());
    }
}

async fn assert_fragmented_meta_control_promotes_to_streaming_deadline(
    session: &str,
    prefix: AttachMessage,
    recognized_opener: AttachMessage,
    completion: AttachMessage,
    expected: &[u8],
) {
    let mut fixture = PendingEscapeSchedulerFixture::start(session).await;
    fixture.send(prefix).await;
    fixture
        .wait_for_capture(|captured| captured == b"A", "prefix dispatch")
        .await;
    fixture.send(recognized_opener).await;

    // The configured escape-time is 500 ms. Once the second fragment selects
    // a recognized OSC/APC family, it must survive beyond that keyboard budget.
    tokio::time::sleep(Duration::from_millis(750)).await;
    assert_eq!(
        fixture
            .handler
            .attached_input_capture_for_test(&fixture.target)
            .await,
        Some(b"A".to_vec()),
        "a transport split after ESC must not flush a recognized control body"
    );

    fixture.send(completion).await;
    fixture
        .wait_for_capture(
            |captured| captured == expected,
            "fragmented Meta control completion",
        )
        .await;
    fixture.finish().await;
}

#[tokio::test]
async fn unix_data_osc_split_after_escape_promotes_to_streaming_deadline() {
    assert_fragmented_meta_control_promotes_to_streaming_deadline(
        "unix-data-split-osc-streaming-deadline",
        AttachMessage::Data(b"A\x1b".to_vec()),
        AttachMessage::Data(b"]52;c;UNIX_OSC".to_vec()),
        AttachMessage::Data(b"\x07Z".to_vec()),
        b"AZ",
    )
    .await;
}

#[tokio::test]
async fn windows_keystroke_apc_split_after_escape_promotes_to_streaming_deadline() {
    assert_fragmented_meta_control_promotes_to_streaming_deadline(
        "windows-keystroke-split-apc-streaming-deadline",
        AttachMessage::Keystroke(AttachedKeystroke::new(b"A\x1b".to_vec())),
        AttachMessage::Keystroke(AttachedKeystroke::new(b"_Gi=7;WINDOWS_APC".to_vec())),
        AttachMessage::Keystroke(AttachedKeystroke::new(b"_BODY\x1b\\Z".to_vec())),
        b"A\x1b_Gi=7;WINDOWS_APC_BODY\x1b\\Z",
    )
    .await;
}

async fn assert_invalid_meta_byte_flushes_on_keyboard_deadline(
    session: &str,
    input: AttachMessage,
) {
    let mut fixture = PendingEscapeSchedulerFixture::start(session).await;
    let started = Instant::now();
    fixture.send(input).await;
    let captured = fixture
        .wait_for_capture(
            |captured| captured == b"A\x1b\xff",
            "invalid Meta byte keyboard-deadline flush",
        )
        .await;
    assert_eq!(captured, b"A\x1b\xff");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "invalid Meta input must use escape-time, not the 8-second stream budget"
    );

    fixture.send(AttachMessage::Data(b"Z".to_vec())).await;
    fixture
        .wait_for_capture(
            |captured| captured == b"A\x1b\xffZ",
            "ordinary input after invalid Meta flush",
        )
        .await;
    fixture.finish().await;
}

#[tokio::test]
async fn invalid_meta_byte_deadline_covers_data_and_windows_keystroke_frames() {
    assert_invalid_meta_byte_flushes_on_keyboard_deadline(
        "invalid-meta-data-deadline",
        AttachMessage::Data(b"A\x1b\xff".to_vec()),
    )
    .await;
    assert_invalid_meta_byte_flushes_on_keyboard_deadline(
        "invalid-meta-keystroke-deadline",
        AttachMessage::Keystroke(AttachedKeystroke::new(b"A\x1b\xff".to_vec())),
    )
    .await;
}

async fn assert_invalid_csi_body_is_forwarded_without_retention(
    session: &str,
    input: AttachMessage,
) {
    let mut fixture = PendingEscapeSchedulerFixture::start(session).await;
    fixture.send(input).await;
    let captured = fixture
        .wait_for_capture(
            |captured| captured == b"A\x1b[1\r",
            "invalid CSI body forwarding",
        )
        .await;
    assert_eq!(captured, b"A\x1b[1\r");
    fixture.finish().await;
}

#[tokio::test]
async fn invalid_csi_body_cannot_be_retained_without_a_deadline() {
    assert_invalid_csi_body_is_forwarded_without_retention(
        "invalid-csi-body-data",
        AttachMessage::Data(b"A\x1b[1\r".to_vec()),
    )
    .await;
    assert_invalid_csi_body_is_forwarded_without_retention(
        "invalid-csi-body-keystroke",
        AttachMessage::Keystroke(AttachedKeystroke::new(b"A\x1b[1\r".to_vec())),
    )
    .await;
}

#[tokio::test]
async fn sustained_ready_socket_serves_the_initial_ambiguous_csi_deadline() {
    let mut fixture = PendingEscapeSchedulerFixture::start("sustained-ready-csi-deadline").await;
    fixture
        .send(AttachMessage::Data(b"A\x1b[12;".to_vec()))
        .await;
    fixture
        .wait_for_capture(|captured| captured == b"A", "ambiguous CSI retention")
        .await;

    // Keep producing immediately readable frames for longer than escape-time.
    // The attach loop must still service the original ambiguity deadline.
    let empty = AttachMessage::Data(Vec::new());
    let started = Instant::now();
    let captured = tokio::time::timeout(Duration::from_millis(900), async {
        loop {
            fixture
                .send_batch(&[
                    empty.clone(),
                    empty.clone(),
                    empty.clone(),
                    empty.clone(),
                    empty.clone(),
                    empty.clone(),
                    empty.clone(),
                    empty.clone(),
                ])
                .await;
            tokio::task::yield_now().await;
            let captured = fixture
                .handler
                .attached_input_capture_for_test(&fixture.target)
                .await
                .expect("input capture remains installed");
            if captured == b"A\x1b[12;" {
                break captured;
            }
        }
    })
    .await
    .expect("continuously ready socket must not starve the CSI deadline");
    assert_eq!(captured, b"A\x1b[12;");
    assert!(started.elapsed() < Duration::from_millis(900));
    fixture.finish().await;
}

async fn assert_streaming_control_survives_keyboard_escape_time(
    session: &str,
    prefix: &[u8],
    completion: &[u8],
) -> Vec<u8> {
    let mut fixture = PendingEscapeSchedulerFixture::start(session).await;
    fixture.send(AttachMessage::Data(prefix.to_vec())).await;
    fixture
        .wait_for_capture(|captured| captured == b"A", "stream prefix dispatch")
        .await;

    tokio::time::sleep(Duration::from_millis(750)).await;
    let before_completion = fixture
        .handler
        .attached_input_capture_for_test(&fixture.target)
        .await;
    assert_eq!(
        before_completion,
        Some(b"A".to_vec()),
        "an unambiguous streaming control must outlive keyboard escape-time"
    );

    fixture.send(AttachMessage::Data(completion.to_vec())).await;
    let captured = fixture
        .wait_for_capture(|captured| captured.ends_with(b"Z"), "stream completion")
        .await;
    fixture.finish().await;
    captured
}

#[tokio::test]
async fn fragmented_osc_control_keeps_the_streaming_idle_deadline() {
    let captured = assert_streaming_control_survives_keyboard_escape_time(
        "fragmented-osc-stream-deadline",
        b"A\x1b]52;c;AA",
        b"AA\x07Z",
    )
    .await;
    assert_eq!(captured, b"AZ");
}

#[tokio::test]
async fn fragmented_apc_control_keeps_the_streaming_idle_deadline() {
    let captured = assert_streaming_control_survives_keyboard_escape_time(
        "fragmented-apc-stream-deadline",
        b"A\x1b_Gi=7;PAY",
        b"LOAD\x1b\\Z",
    )
    .await;
    assert_eq!(captured, b"A\x1b_Gi=7;PAYLOAD\x1b\\Z");
}

#[tokio::test]
async fn socket_dispatch_rearms_replaced_same_kind_ambiguous_suffix() {
    let (live_input, stream, _peer, _control_rx) =
        pending_escape_socket_fixture("escape-epoch-ambiguous").await;
    let mut decoder = AttachFrameDecoder::new();
    let mut pending_input = Vec::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut active_emit_cache = None;
    let mut locked = false;

    decoder.push_bytes(
        &encode_attach_message(&AttachMessage::Data(b"\x1b".to_vec()))
            .expect("encode initial Escape"),
    );
    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("retain initial Escape");
    assert_eq!(pending_input, b"\x1b");
    sync_pending_escape_flush_with_escape_time(
        &mut pending_escape_flush,
        &pending_input,
        Duration::from_secs(1),
    );
    let first_deadline = pending_escape_flush
        .deadline()
        .expect("initial Escape arms a deadline");

    decoder.push_bytes(
        &encode_attach_message(&AttachMessage::Data(b"x\x1b".to_vec()))
            .expect("encode replacement Escape"),
    );
    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("consume Meta-x and retain replacement Escape");
    assert_eq!(pending_input, b"\x1b");
    sync_pending_escape_flush_with_escape_time(
        &mut pending_escape_flush,
        &pending_input,
        Duration::from_secs(3),
    );
    let replacement_deadline = pending_escape_flush
        .deadline()
        .expect("replacement Escape arms a fresh deadline");

    assert!(
        replacement_deadline > first_deadline + Duration::from_secs(1),
        "a same-kind suffix must not inherit the consumed prefix's deadline"
    );
}

#[tokio::test]
async fn socket_dispatch_promotes_coalesced_split_osc_to_streaming() {
    let (live_input, stream, _peer, _control_rx) =
        pending_escape_socket_fixture("escape-meta-osc-provenance").await;
    let mut decoder = AttachFrameDecoder::new();
    let mut pending_input = Vec::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut active_emit_cache = None;
    let mut locked = false;
    let mut encoded = encode_attach_message(&AttachMessage::Data(b"A\x1b".to_vec()))
        .expect("encode initial Meta escape");
    encoded.extend_from_slice(
        &encode_attach_message(&AttachMessage::Data(b"]52;c;COALESCED".to_vec()))
            .expect("encode OSC-like continuation"),
    );
    decoder.push_bytes(&encoded);

    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("retain coalesced OSC-like Meta input");
    assert_eq!(pending_input, b"\x1b]52;c;COALESCED");

    let before = Instant::now();
    sync_pending_escape_flush_with_escape_time(
        &mut pending_escape_flush,
        &pending_input,
        Duration::from_millis(500),
    );
    let deadline = pending_escape_flush
        .deadline()
        .expect("coalesced split OSC input must arm");
    assert!(
        deadline >= before + Duration::from_secs(8),
        "a recognized OSC opener must promote beyond the initial Meta ambiguity"
    );
}

#[tokio::test]
async fn socket_dispatch_preserves_deadline_for_true_csi_continuation() {
    let (live_input, stream, _peer, _control_rx) =
        pending_escape_socket_fixture("escape-epoch-continuation").await;
    let mut decoder = AttachFrameDecoder::new();
    let mut pending_input = Vec::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut active_emit_cache = None;
    let mut locked = false;

    decoder.push_bytes(
        &encode_attach_message(&AttachMessage::Data(b"\x1b[".to_vec()))
            .expect("encode initial CSI opener"),
    );
    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("retain initial CSI opener");
    assert_eq!(pending_input, b"\x1b[");
    sync_pending_escape_flush_with_escape_time(
        &mut pending_escape_flush,
        &pending_input,
        Duration::from_secs(1),
    );
    let original_deadline = pending_escape_flush
        .deadline()
        .expect("initial CSI opener arms a deadline");

    decoder.push_bytes(
        &encode_attach_message(&AttachMessage::Data(b"12".to_vec()))
            .expect("encode continued CSI parameters"),
    );
    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("retain continued CSI parameters");
    assert_eq!(pending_input, b"\x1b[12");
    sync_pending_escape_flush_with_escape_time(
        &mut pending_escape_flush,
        &pending_input,
        Duration::from_secs(30),
    );

    assert_eq!(
        pending_escape_flush.deadline(),
        Some(original_deadline),
        "a true continuation must not turn keyboard escape-time into a sliding deadline"
    );
}

#[tokio::test]
async fn socket_dispatch_rearms_replaced_same_length_streaming_suffix() {
    let (live_input, stream, _peer, _control_rx) =
        pending_escape_socket_fixture("escape-epoch-streaming").await;
    let mut decoder = AttachFrameDecoder::new();
    let mut pending_input = Vec::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut active_emit_cache = None;
    let mut locked = false;
    let incomplete_paste = b"\x1b[200~body";

    decoder.push_bytes(
        &encode_attach_message(&AttachMessage::Data(incomplete_paste.to_vec()))
            .expect("encode initial incomplete paste"),
    );
    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("retain initial incomplete paste");
    assert_eq!(pending_input, incomplete_paste);
    sync_pending_escape_flush_with_escape_time(
        &mut pending_escape_flush,
        &pending_input,
        Duration::from_secs(8),
    );
    let first_deadline = pending_escape_flush
        .deadline()
        .expect("initial paste stream arms a deadline");

    let mut replacement = b"\x1b[201~".to_vec();
    replacement.extend_from_slice(incomplete_paste);
    decoder.push_bytes(
        &encode_attach_message(&AttachMessage::Data(replacement))
            .expect("encode completed and replacement paste streams"),
    );
    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("complete first paste and retain replacement stream");
    assert_eq!(
        pending_input, incomplete_paste,
        "the replacement intentionally matches the old kind, length, and contents"
    );
    sync_pending_escape_flush_with_escape_time(
        &mut pending_escape_flush,
        &pending_input,
        Duration::from_secs(30),
    );
    let replacement_deadline = pending_escape_flush
        .deadline()
        .expect("replacement paste stream arms a fresh deadline");

    assert!(
        replacement_deadline > first_deadline + Duration::from_secs(20),
        "a same-length streaming suffix must not inherit the completed stream's deadline"
    );
}

#[test]
fn overlay_generation_rejects_stale_clears_after_switches_or_newer_overlays() {
    let mut current_overlay_generation = 0;

    assert!(should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 1)
    ));
    assert_eq!(current_overlay_generation, 1);

    assert!(should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 1)
    ));
    assert!(should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 2)
    ));

    assert!(!should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 1)
    ));
    assert!(!should_emit_overlay(
        1,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 3)
    ));
    assert_eq!(current_overlay_generation, 2);

    assert!(should_emit_overlay(
        1,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 2, 3)
    ));
    assert_eq!(current_overlay_generation, 3);

    assert!(!should_emit_overlay(
        2,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 1, 4)
    ));
}

#[test]
fn target_change_clears_deferred_pane_output_close() {
    let mut close_after_refresh = true;

    clear_close_pane_output_after_refresh_if_target_changed(true, &mut close_after_refresh);

    assert!(
        !close_after_refresh,
        "a deferred close belongs to the old attach target and must not apply after a switch"
    );
}

#[test]
fn same_target_keeps_deferred_pane_output_close() {
    let mut close_after_refresh = true;

    clear_close_pane_output_after_refresh_if_target_changed(false, &mut close_after_refresh);

    assert!(close_after_refresh);
}

#[test]
fn predicted_local_echo_accepts_only_single_printable_bytes() {
    assert!(is_predictable_local_echo(b"a"));
    assert!(is_predictable_local_echo(b"abc123"));
    assert!(is_predictable_local_echo(b" "));
    assert!(is_predictable_local_echo(b"~"));
    assert!(!is_predictable_local_echo(b"\n"));
    assert!(!is_predictable_local_echo(b"\x1b"));
    assert!(!is_predictable_local_echo(b"0123456789abcdefg"));
    assert!(!is_predictable_local_echo("é".as_bytes()));
}

#[test]
fn predicted_local_echo_accepts_printable_prefix_before_enter() {
    assert_eq!(predictable_local_echo_prefix_len(b"PING123\r"), 7);
    assert_eq!(predictable_local_echo_prefix_len(b"PING123\n"), 7);
    assert_eq!(predictable_local_echo_prefix_len(b"PING123\t"), 0);
    assert_eq!(predictable_local_echo_prefix_len(b"\r"), 0);
}

#[test]
fn predicted_local_echo_consumes_exact_pty_echo_once() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let mut target =
        open_attach_target(test_attach_target(&alpha, b"", None), false).expect("open target");

    target.predicted_echo.extend(b"xyz");
    assert_eq!(
        consume_predicted_echo(&mut target, b"xyz"),
        PredictedEcho::Consumed
    );
    assert!(target.predicted_echo.is_empty());

    target.predicted_echo.extend(b"x");
    assert_eq!(
        consume_predicted_echo(&mut target, b"y"),
        PredictedEcho::Mismatch
    );
    assert!(target.predicted_echo.is_empty());

    target.predicted_echo.extend(b"x");
    assert_eq!(
        consume_predicted_echo(&mut target, b"xy"),
        PredictedEcho::Mismatch
    );
    assert!(target.predicted_echo.is_empty());
}

#[test]
fn stale_predicted_local_echo_expires_without_pty_echo() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let mut target =
        open_attach_target(test_attach_target(&alpha, b"", None), false).expect("open target");

    target.predicted_echo.extend(b"secret");
    target.predicted_echo_started_at =
        Some(Instant::now() - super::PREDICTED_LOCAL_ECHO_TIMEOUT * 2);

    assert_eq!(
        consume_predicted_echo(&mut target, b"visible"),
        PredictedEcho::NoPrediction
    );
    assert!(target.predicted_echo.is_empty());
    assert!(target.predicted_echo_started_at.is_none());
}

#[tokio::test]
async fn live_render_frame_uses_render_message_for_capable_clients() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let target = test_attach_target(&alpha, b"", None);
    let mut target = open_attach_target(target, true).expect("open attach target");
    let frame = PaneRenderDeltaFrame::new(b"live".to_vec(), None);
    let (stream, mut peer) = tokio::io::duplex(1024);
    let stream = AttachTransport::from_io(stream);

    super::emit_live_render_frame(&stream, &mut target, &frame, true)
        .await
        .expect("emit live render frame");

    let mut bytes = [0_u8; 128];
    let count = peer.read(&mut bytes).await.expect("read emitted frame");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&bytes[..count]);

    assert!(matches!(
        decoder.next_message().expect("decode emitted frame"),
        Some(AttachMessage::Render(bytes)) if bytes.ends_with(b"live")
    ));
}

#[tokio::test]
async fn live_render_delta_uses_data_message_for_stateful_frames() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let target = test_attach_target(&alpha, b"", None);
    let mut target = open_attach_target(target, true).expect("open attach target");
    let frame = PaneRenderDeltaFrame::new(b"delta".to_vec(), None);
    let (stream, mut peer) = tokio::io::duplex(1024);
    let stream = AttachTransport::from_io(stream);

    super::emit_live_render_frame(&stream, &mut target, &frame, false)
        .await
        .expect("emit live render frame");

    let mut bytes = [0_u8; 128];
    let count = peer.read(&mut bytes).await.expect("read emitted frame");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&bytes[..count]);

    assert!(matches!(
        decoder.next_message().expect("decode emitted frame"),
        Some(AttachMessage::Data(bytes)) if bytes.ends_with(b"delta")
    ));
}

#[tokio::test]
async fn pane_output_receiver_reports_lag_and_resumes_from_oldest_retained_event() {
    let sender = pane_output_channel_with_limits(1, 32);
    let mut receiver = sender.subscribe();

    sender.send(b"first".to_vec());
    sender.send(b"second".to_vec());

    let OutputCursorItem::Gap(gap) = recv_pane_output(&mut receiver)
        .await
        .expect("receive explicit output gap")
    else {
        panic!("slow receiver should observe a cursor gap");
    };
    assert_eq!(gap.expected_sequence(), 0);
    assert_eq!(gap.resume_sequence(), 1);
    assert_eq!(gap.missed_events(), 1);
    assert_eq!(gap.missed_range(), 0..1);
    assert_eq!(gap.recent_snapshot().bytes(), b"firstsecond");
    assert_eq!(gap.recent_snapshot().oldest_sequence(), Some(0));
    assert_eq!(gap.recent_snapshot().newest_sequence(), Some(1));

    let OutputCursorItem::Event(event) = recv_pane_output(&mut receiver)
        .await
        .expect("receive oldest retained output event")
    else {
        panic!("receiver should resume with the oldest retained event");
    };
    assert_eq!(event.sequence(), 1);
    assert_eq!(event.bytes(), b"second");
}

#[tokio::test]
async fn typed_keystroke_wire_reaches_stub_and_acknowledges() {
    let proof_root =
        std::env::temp_dir().join(format!("rmux-step02-protocol-{}", std::process::id()));
    std::fs::create_dir_all(&proof_root).expect("create /tmp check root");

    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name, control_tx)
        .await;

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let keystroke = AttachedKeystroke::new(b"\x1b[A".to_vec());
    let encoded = encode_attach_message(&AttachMessage::Keystroke(keystroke))
        .expect("encode typed keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);
    let mut pending_input = Vec::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut active_emit_cache = None;
    let mut locked = true;
    let live_input = LiveAttachInputContext::current_for_test(handler, attach_pid).await;

    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("process typed keystroke");

    let mut ack_bytes = [0_u8; 64];
    let bytes_read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut ack_bytes))
        .await
        .expect("ack read should not time out")
        .expect("read ack");
    let mut ack_decoder = AttachFrameDecoder::new();
    ack_decoder.push_bytes(&ack_bytes[..bytes_read]);
    assert_eq!(
        ack_decoder.next_message().expect("decode ack"),
        Some(AttachMessage::KeyDispatched(KeyDispatched::new(3)))
    );

    std::fs::remove_dir_all(proof_root).expect("remove /tmp check root");
}

#[tokio::test]
async fn mouse_keystroke_wire_does_not_error_or_drop_the_attach() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("alpha").expect("valid session name");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name, control_tx)
        .await;

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let keystroke = AttachedKeystroke::new(b"\x1b[<0;10;10M".to_vec());
    let encoded = encode_attach_message(&AttachMessage::Keystroke(keystroke))
        .expect("encode mouse keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);
    let mut pending_input = Vec::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut active_emit_cache = None;
    let mut locked = false;
    let live_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;

    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        PendingAttachInputState::new(&mut pending_input, &mut pending_escape_flush),
        &mut active_emit_cache,
        &mut locked,
    )
    .await
    .expect("process mouse keystroke");

    let mut ack_bytes = [0_u8; 128];
    let bytes_read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut ack_bytes))
        .await
        .expect("ack read should not time out")
        .expect("read ack");
    let mut ack_decoder = AttachFrameDecoder::new();
    ack_decoder.push_bytes(&ack_bytes[..bytes_read]);
    assert_eq!(
        ack_decoder.next_message().expect("decode ack"),
        Some(AttachMessage::KeyDispatched(KeyDispatched::new(11)))
    );
}

#[tokio::test]
async fn data_payload_does_not_trust_an_unversioned_cached_pane_master() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("cached-master").expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    handler.start_attached_input_capture_for_test(&target).await;

    // This master has the same logical target spelling but is deliberately
    // unrelated to the current pane lifetime, as happens while a respawn
    // switch control is still queued.
    let mut cached_target = open_attach_target(test_attach_target(&session_name, b"", None), false)
        .expect("open stale cached target");
    let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let live_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    let mut pending_input = Vec::new();
    let mut active_emit_cache = None;
    let mut locked = false;

    let forwarded = process_attach_data_payload(
        &live_input,
        &stream,
        Some(&mut cached_target),
        &mut pending_input,
        &mut active_emit_cache,
        &mut locked,
        b"SAFE",
    )
    .await
    .expect("data payload routes through the current handler state");

    assert!(forwarded);
    assert!(pending_input.is_empty());
    assert_eq!(
        handler.attached_input_capture_for_test(&target).await,
        Some(b"SAFE".to_vec())
    );
}

#[tokio::test]
async fn forward_attach_emits_stop_sequence_when_processing_errors() {
    let handler = Arc::new(RequestHandler::new());
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let outer_terminal =
        OuterTerminal::resolve(&OptionStore::default(), OuterTerminalContext::default());
    let expected_stop = outer_terminal.attach_stop_sequence();
    let pane_output = pane_output_channel();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name: SessionName::new("alpha").expect("valid session name"),
        pane_master: Some(pane_master),
        pane_output,
        pane_output_start_sequence,
        render_frame: Vec::new(),
        outer_terminal,
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
    };
    let invalid_initial_socket_bytes =
        encode_attach_message(&AttachMessage::Lock("unexpected".to_owned()))
            .expect("encode unexpected lock frame");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let result = forward_attach(
        stream,
        target,
        invalid_initial_socket_bytes,
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    )
    .await;
    assert!(result.is_err(), "invalid attach input should fail");

    let mut collected = Vec::new();
    let mut frame_bytes = [0_u8; 4096];
    loop {
        let bytes_read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut frame_bytes))
            .await
            .expect("peer read should not time out")
            .expect("read peer bytes");
        if bytes_read == 0 {
            break;
        }
        let mut decoder = AttachFrameDecoder::new();
        decoder.push_bytes(&frame_bytes[..bytes_read]);
        while let Some(message) = decoder.next_message().expect("decode attach frame") {
            if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
                collected.extend_from_slice(&bytes);
            }
        }
    }

    assert!(
        collected
            .windows(expected_stop.len())
            .any(|window| window == expected_stop),
        "attach stop sequence should be emitted on attach failure"
    );
}

#[tokio::test]
async fn detach_control_emits_stop_and_banner_in_one_data_frame() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target = open_attach_target(test_attach_target(&alpha, b"BASE-A", None), false)
        .expect("open target");
    let expected_stop = current_target.outer_terminal.attach_stop_sequence();
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    control_tx
        .send(AttachControl::Detach)
        .expect("send detach control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply pending detach");

    assert!(matches!(action, PendingAttachAction::Exit(_)));

    let mut frame_bytes = [0_u8; 4096];
    let bytes_read = peer
        .read(&mut frame_bytes)
        .await
        .expect("read detach frame");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&frame_bytes[..bytes_read]);
    let Some(AttachMessage::Data(bytes)) = decoder.next_message().expect("decode detach frame")
    else {
        panic!("detach should emit a data frame");
    };

    assert!(
        bytes
            .windows(expected_stop.len())
            .any(|window| window == expected_stop),
        "detach data must contain attach-stop before close"
    );
    assert!(
        bytes
            .windows(b"[detached (from session alpha)]\r\n".len())
            .any(|window| window == b"[detached (from session alpha)]\r\n"),
        "detach data must contain detached banner"
    );
}

#[tokio::test]
async fn lock_control_emits_attach_stop_before_transferring_terminal_ownership() {
    let alpha = SessionName::new("alpha-lock-stop").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target =
        open_attach_target(test_attach_target(&alpha, b"BASE", None), false).expect("open target");
    let expected_stop = current_target.outer_terminal.attach_stop_sequence();
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();
    let command = AttachShellCommand::new(
        "lock-command".to_owned(),
        "/bin/sh".to_owned(),
        "/tmp".to_owned(),
    );
    control_tx
        .send(AttachControl::LockShellCommand(command.clone()))
        .expect("send lock control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply pending lock");
    assert!(matches!(action, PendingAttachAction::Continue { .. }));
    assert!(locked);

    let messages = tokio::time::timeout(Duration::from_secs(1), async {
        let mut decoder = AttachFrameDecoder::new();
        let mut messages = Vec::new();
        let mut bytes = [0_u8; 4096];
        while messages.len() < 2 {
            let read = peer.read(&mut bytes).await.expect("read lock frames");
            assert!(read > 0, "attach stream closed before lock frames");
            decoder.push_bytes(&bytes[..read]);
            while let Some(message) = decoder.next_message().expect("decode lock frame") {
                messages.push(message);
            }
        }
        messages
    })
    .await
    .expect("lock frames timed out");

    let AttachMessage::Data(stop) = &messages[0] else {
        panic!(
            "first lock frame must restore the outer terminal: {:?}",
            messages[0]
        );
    };
    assert!(
        stop.windows(expected_stop.len())
            .any(|window| window == expected_stop),
        "lock must emit the complete attach-stop sequence first"
    );
    assert_eq!(messages[1], AttachMessage::LockShellCommand(command));
}

fn test_attach_target(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
) -> AttachTarget {
    test_attach_target_with_output(
        session_name,
        render_frame,
        persistent_overlay_state_id,
        pane_output_channel(),
        false,
    )
}

fn test_attach_target_with_output(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
    pane_output: super::types::PaneOutputSender,
    kitty_graphics_passthrough: bool,
) -> AttachTarget {
    test_attach_target_with_protocols(
        session_name,
        render_frame,
        persistent_overlay_state_id,
        pane_output,
        kitty_graphics_passthrough,
        false,
    )
}

fn test_attach_target_with_protocols(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
    pane_output: super::types::PaneOutputSender,
    kitty_graphics_passthrough: bool,
    sixel_passthrough: bool,
) -> AttachTarget {
    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    AttachTarget {
        session_name: session_name.clone(),
        pane_master: Some(pane_master),
        pane_output,
        pane_output_start_sequence,
        render_frame: render_frame.to_vec(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: kitty_graphics_passthrough || sixel_passthrough,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id,
        live_pane: None,
    }
}

fn test_render_only_attach_target(session_name: &SessionName, render_frame: &[u8]) -> AttachTarget {
    test_render_only_attach_target_with_state(session_name, render_frame, None)
}

fn test_render_only_attach_target_with_state(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
) -> AttachTarget {
    let mut target = test_attach_target(session_name, render_frame, persistent_overlay_state_id);
    target.pane_master = None;
    target
}

#[test]
fn render_only_switches_coalesce_before_reliable_controls() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut deferred_controls = VecDeque::new();

    let first = test_render_only_attach_target(&alpha, b"first");
    let second = test_render_only_attach_target(&alpha, b"second");
    let third = test_render_only_attach_target(&alpha, b"third");
    control_tx
        .send(AttachControl::switch(second))
        .expect("queue second switch");
    control_tx
        .send(AttachControl::switch(third))
        .expect("queue third switch");
    control_tx
        .send(AttachControl::Detach)
        .expect("queue reliable detach");

    let control_backlog = AtomicUsize::new(0);
    let (coalesced, switch_count) = coalesce_render_switches(
        Box::new(first),
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
    );

    assert_eq!(coalesced.render_frame, b"third");
    assert_eq!(switch_count, 3);
    assert!(matches!(
        deferred_controls.pop_front(),
        Some(AttachControl::Detach)
    ));
}

#[test]
fn render_only_switch_coalescing_preserves_deferred_control_order() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut deferred_controls = VecDeque::from([AttachControl::Refresh]);

    let first = test_render_only_attach_target(&alpha, b"first");
    let second = test_render_only_attach_target(&alpha, b"second");
    control_tx
        .send(AttachControl::switch(second))
        .expect("queue render switch");
    control_tx
        .send(AttachControl::Detach)
        .expect("queue reliable detach");

    let control_backlog = AtomicUsize::new(0);
    let (coalesced, switch_count) = coalesce_render_switches(
        Box::new(first),
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
    );

    assert_eq!(coalesced.render_frame, b"second");
    assert_eq!(switch_count, 2);
    assert!(matches!(
        deferred_controls.pop_front(),
        Some(AttachControl::Refresh)
    ));
    assert!(matches!(
        deferred_controls.pop_front(),
        Some(AttachControl::Detach)
    ));
}

#[tokio::test]
async fn pending_switch_action_reports_target_change_for_status_reschedule() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let beta = SessionName::new("beta").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target = open_attach_target(test_attach_target(&alpha, b"BASE-A", None), false)
        .expect("open target");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &beta, b"BASE-B", None,
        )))
        .expect("send switch control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply pending switch");

    assert!(matches!(
        action,
        PendingAttachAction::Continue {
            target_changed: true
        }
    ));
    assert_eq!(current_target.session_name, beta);
    let refresh = read_attach_data_until(&mut peer, b"BASE-B").await;
    assert!(
        String::from_utf8_lossy(&refresh).contains("BASE-B"),
        "switch should render the target frame"
    );
}

#[tokio::test]
async fn pending_refresh_after_switch_preserves_target_change() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let beta = SessionName::new("beta").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target = open_attach_target(test_attach_target(&alpha, b"BASE-A", None), false)
        .expect("open target");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &beta, b"BASE-B", None,
        )))
        .expect("send switch control");
    control_tx
        .send(AttachControl::Refresh)
        .expect("send refresh control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply pending switch and refresh");

    assert!(matches!(
        action,
        PendingAttachAction::Refresh {
            target_changed: true
        }
    ));
    assert_eq!(current_target.session_name, beta);
    let refresh = read_attach_data_until(&mut peer, b"BASE-B").await;
    assert!(
        String::from_utf8_lossy(&refresh).contains("BASE-B"),
        "switch should render before the refresh is scheduled"
    );
}

#[tokio::test]
async fn pending_same_pane_switch_preserves_partial_input_and_escape_deadline() {
    let alpha = SessionName::new("pending-input-refresh").expect("valid session name");
    let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (_control_tx, mut control_rx) = mpsc::unbounded_channel();
    let pane_output = pane_output_channel();
    let mut current_target = open_attach_target(
        test_attach_target_with_output(&alpha, b"BASE-A", None, pane_output.clone(), false),
        false,
    )
    .expect("open target");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut pending_input = b"\x1b_".to_vec();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    pending_escape_flush.sync(&pending_input, Duration::from_secs(30));
    let original_deadline = pending_escape_flush
        .deadline()
        .expect("Meta-_ should arm the escape deadline");
    let mut deferred_controls = VecDeque::from([AttachControl::switch(
        test_attach_target_with_output(&alpha, b"BASE-B", None, pane_output, false),
    )]);

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        Some(PendingAttachInputState::new(
            &mut pending_input,
            &mut pending_escape_flush,
        )),
    )
    .await
    .expect("apply queued same-pane refresh");

    assert!(matches!(action, PendingAttachAction::Continue { .. }));
    assert_eq!(pending_input, b"\x1b_");
    assert_eq!(pending_escape_flush.deadline(), Some(original_deadline));
}

#[tokio::test]
async fn pending_different_pane_switch_clears_partial_input_and_escape_deadline() {
    let alpha = SessionName::new("pending-input-pane-change").expect("valid session name");
    let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (_control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target = open_attach_target(test_attach_target(&alpha, b"BASE-A", None), false)
        .expect("open target");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut pending_input = b"\x1b_".to_vec();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    pending_escape_flush.sync(&pending_input, Duration::from_secs(30));
    let mut deferred_controls = VecDeque::from([AttachControl::switch(test_attach_target(
        &alpha, b"BASE-B", None,
    ))]);

    let control_backlog = AtomicUsize::new(0);
    apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        Some(PendingAttachInputState::new(
            &mut pending_input,
            &mut pending_escape_flush,
        )),
    )
    .await
    .expect("apply queued pane change");

    assert!(pending_input.is_empty());
    assert!(pending_escape_flush.deadline().is_none());
}

#[tokio::test]
async fn terminal_ownership_controls_clear_partial_input_and_escape_deadline() {
    for (label, control) in [
        (
            "lock",
            AttachControl::LockShellCommand(AttachShellCommand::new(
                "lock-command".to_owned(),
                "/bin/sh".to_owned(),
                "/tmp".to_owned(),
            )),
        ),
        ("suspend", AttachControl::Suspend),
    ] {
        let session_name =
            SessionName::new(format!("pending-input-{label}")).expect("valid session name");
        let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
        let stream = AttachTransport::from(stream);
        let (_control_tx, mut control_rx) = mpsc::unbounded_channel();
        let mut current_target =
            open_attach_target(test_attach_target(&session_name, b"BASE", None), false)
                .expect("open target");
        let mut render_generation = 0_u64;
        let mut overlay_generation = 0_u64;
        let mut persistent_overlay = None::<Vec<u8>>;
        let mut persistent_overlay_visible = false;
        let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
        let mut locked = false;
        let mut pending_input = b"\x1b_".to_vec();
        let mut pending_escape_flush = PendingEscapeFlush::default();
        pending_escape_flush.sync(&pending_input, Duration::from_secs(30));
        assert!(pending_escape_flush.deadline().is_some());
        let mut deferred_controls = VecDeque::from([control]);

        let control_backlog = AtomicUsize::new(0);
        let action = apply_pending_attach_controls(
            &mut deferred_controls,
            Some(&mut control_rx),
            &control_backlog,
            &mut current_target,
            &stream,
            &mut render_generation,
            &mut overlay_generation,
            &mut persistent_overlay,
            &mut persistent_overlay_visible,
            &mut persistent_overlay_state_id,
            &mut locked,
            Some(PendingAttachInputState::new(
                &mut pending_input,
                &mut pending_escape_flush,
            )),
        )
        .await
        .unwrap_or_else(|error| panic!("apply pending {label}: {error}"));

        assert!(
            matches!(action, PendingAttachAction::Continue { .. }),
            "{label} transfers terminal ownership"
        );
        assert!(locked, "{label} marks the attach as locked");
        assert!(pending_input.is_empty(), "{label} drops partial input");
        assert!(
            pending_escape_flush.deadline().is_none(),
            "{label} cancels the stale escape deadline"
        );
    }
}

#[tokio::test]
async fn stale_persistent_switches_still_advance_render_generation() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let beta = SessionName::new("beta").expect("valid session name");
    let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target =
        open_attach_target(test_attach_target(&alpha, b"BASE-A", Some(10)), false)
            .expect("open target");
    let mut render_generation = 41_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &beta,
            b"STALE-B",
            Some(9),
        )))
        .expect("send stale switch control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply stale pending switch");

    assert!(matches!(action, PendingAttachAction::Write));
    assert_eq!(current_target.session_name, alpha);
    assert_eq!(render_generation, 42);
}

#[tokio::test]
async fn render_only_switch_forwards_pending_live_passthroughs() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let pane_output = pane_output_channel();
    let mut initial =
        test_attach_target_with_output(&alpha, b"BASE-A", None, pane_output.clone(), true);
    initial.pane_master = None;
    let mut replacement =
        test_attach_target_with_output(&alpha, b"BASE-B", None, pane_output.clone(), true);
    replacement.pane_master = None;
    let mut current_target = open_attach_target(initial, false).expect("open target");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    pane_output.send_for_generation_with_passthroughs(
        None,
        b"image".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;AAAA".to_vec(),
        )],
    );
    pane_output.send_for_generation_with_passthroughs(
        None,
        b"next-image".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;BBBB".to_vec(),
        )],
    );
    replacement.pane_output_start_sequence = 1;
    control_tx
        .send(AttachControl::switch(replacement))
        .expect("send render-only switch");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply pending switch");

    assert!(matches!(action, PendingAttachAction::Write));
    let refresh = read_attach_data_until(&mut peer, b"Gf=100;AAAA").await;
    assert!(
        String::from_utf8_lossy(&refresh).contains("BASE-B"),
        "render-only switch should still write the replacement frame"
    );
    assert!(
        refresh
            .windows(b"\x1b_Gf=100;AAAA\x1b\\".len())
            .any(|window| window == b"\x1b_Gf=100;AAAA\x1b\\"),
        "render-only switch must not drop pending live passthroughs"
    );
    assert!(
        !refresh
            .windows(b"\x1b_Gf=100;BBBB\x1b\\".len())
            .any(|window| window == b"\x1b_Gf=100;BBBB\x1b\\"),
        "render-only switch must not duplicate passthroughs covered by the replacement receiver"
    );
}

async fn read_attach_data_until(peer: &mut tokio::net::UnixStream, needle: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut collected = Vec::new();
        let mut frame_bytes = [0_u8; 4096];
        let mut decoder = AttachFrameDecoder::new();
        loop {
            let bytes_read = peer.read(&mut frame_bytes).await.expect("read peer bytes");
            assert!(bytes_read > 0, "attach stream closed before expected data");
            decoder.push_bytes(&frame_bytes[..bytes_read]);
            while let Some(message) = decoder.next_message().expect("decode attach frame") {
                if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
                    collected.extend_from_slice(&bytes);
                }
            }
            if collected
                .windows(needle.len())
                .any(|window| window == needle)
            {
                break collected;
            }
        }
    })
    .await
    .expect("timed out waiting for attach data")
}

#[tokio::test]
async fn forward_attach_exited_control_wins_over_closing_shutdown() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::clone(&closing),
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::Refresh)
        .expect("queue non-terminal control");
    control_tx
        .send(AttachControl::Exited)
        .expect("send exited control");
    closing.store(true, Ordering::SeqCst);
    shutdown_tx.send(()).expect("request attach shutdown");

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    assert!(
        exited
            .windows(b"[exited]\r\n".len())
            .any(|window| window == b"[exited]\r\n"),
        "exited control must win over the closing shutdown race"
    );

    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should exit cleanly: {result:?}"
    );
}

#[tokio::test]
async fn last_session_exit_waits_for_attach_wire_drain_before_daemon_shutdown() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("attach-drain").expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let (daemon_shutdown, mut daemon_shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(daemon_shutdown);
    let forwarder_guard = handler.begin_attach_forwarder();
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let attach_pid = std::process::id();
    let attach_id = handler
        .register_attach_with_closing(
            attach_pid,
            session_name.clone(),
            control_tx,
            Arc::clone(&closing),
            OuterTerminalContext::default(),
            crate::client_flags::ClientFlags::default(),
        )
        .await;
    let live_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));
    let _ = read_attach_data_until(&mut peer, b"BASE-0").await;

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    assert!(
        !handler.request_shutdown_if_pending(),
        "exit-empty must wait for the attached exit frame to drain"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut daemon_shutdown_rx)
            .await
            .is_err(),
        "daemon shutdown must stay pending while the attach forwarder owns the wire"
    );

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    assert!(
        exited
            .windows(b"[exited]\r\n".len())
            .any(|window| window == b"[exited]\r\n"),
        "the terminal exit frame must arrive before daemon shutdown"
    );
    let result = attach_task.await.expect("attach task join");
    assert!(result.is_ok(), "forward_attach should drain: {result:?}");
    handler.finish_attach(attach_pid, attach_id).await;
    drop(forwarder_guard);
    let _ = handler.request_shutdown_if_pending();
    tokio::time::timeout(Duration::from_millis(500), daemon_shutdown_rx)
        .await
        .expect("daemon should shut down after the attach exit frame drains")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn forward_attach_exited_control_drains_final_output_and_passthrough_before_banner() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("exit-drain").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let pane_output = pane_output_channel();
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target_with_output(&session_name, b"BASE-0", None, pane_output.clone(), true),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let _initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    #[cfg(windows)]
    {
        control_tx
            .send(AttachControl::Exited)
            .expect("send exited control");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = pane_output.send_for_generation_with_passthroughs(
        None,
        b"FINAL_TAIL".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;TAIL".to_vec(),
        )],
    );
    let _ = pane_output.send_for_generation(None, Vec::new());
    #[cfg(not(windows))]
    control_tx
        .send(AttachControl::Exited)
        .expect("send exited control");

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    let tail = exited
        .windows(b"FINAL_TAIL".len())
        .position(|window| window == b"FINAL_TAIL")
        .expect("final pane output must be delivered");
    let passthrough = exited
        .windows(b"\x1b_Gf=100;TAIL\x1b\\".len())
        .position(|window| window == b"\x1b_Gf=100;TAIL\x1b\\")
        .expect("final passthrough must be delivered");
    let banner = exited
        .windows(b"[exited]\r\n".len())
        .position(|window| window == b"[exited]\r\n")
        .expect("exit banner must be delivered");
    assert!(tail < banner);
    assert!(passthrough < banner);

    assert!(attach_task.await.expect("attach task join").is_ok());
}

#[tokio::test]
async fn session_exit_before_input_validation_still_drains_final_output() {
    let handler = Arc::new(RequestHandler::new());
    let pane_target =
        create_attach_input_test_session(&handler, "exit-during-validated-input").await;
    let session_name = pane_target.session_name().clone();
    let attach_pid = 912_044;
    let closing = Arc::new(AtomicBool::new(false));
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_closing(
            attach_pid,
            session_name.clone(),
            control_tx,
            Arc::clone(&closing),
            OuterTerminalContext::default(),
            crate::client_flags::ClientFlags::default(),
        )
        .await;
    let live_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    let pause = install_live_attach_input_validation_pause(live_input.identity);
    let pane_output = pane_output_channel();
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target_with_output(&session_name, b"BASE-0", None, pane_output.clone(), false),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));
    let _initial = read_attach_data_until(&mut peer, b"BASE-0").await;

    peer.write_all(
        &encode_attach_message(&AttachMessage::Data(b"RACING_INPUT".to_vec()))
            .expect("encode attach input"),
    )
    .await
    .expect("write racing input");
    tokio::time::timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("input reaches the pre-validation pause");

    let _ = pane_output.send_for_generation(None, b"FINAL_AFTER_CLOSE".to_vec());
    let _ = pane_output.send_for_generation(None, Vec::new());
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    pause.release.notify_one();

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    let tail = exited
        .windows(b"FINAL_AFTER_CLOSE".len())
        .position(|window| window == b"FINAL_AFTER_CLOSE")
        .expect("final output must survive the concurrent input close");
    let banner = exited
        .windows(b"[exited]\r\n".len())
        .position(|window| window == b"[exited]\r\n")
        .expect("exit banner must be delivered");
    assert!(tail < banner, "final output must precede the exit banner");
    assert!(
        attach_task.await.expect("attach task join").is_ok(),
        "terminal close must outrank stale input after it is published"
    );
}

#[tokio::test]
async fn finish_attach_exit_forwards_an_already_dequeued_batch_before_banner() {
    let session_name = SessionName::new("dequeued-exit-drain").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let mut current_target = open_attach_target(
        test_attach_target_with_output(&session_name, b"BASE-0", None, pane_output_channel(), true),
        false,
    )
    .expect("open attach target");
    let mut deferred_passthroughs = Vec::new();

    finish_pending_attach_exit_with_batch(
        AttachExitReason::AttachControlExited,
        &stream,
        &mut current_target,
        &mut deferred_passthroughs,
        Some(AttachOutputBatch::Events {
            bytes: b"DEQUEUED_FINAL_TAIL".to_vec(),
            passthroughs: vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                b"Gf=100;DEQUEUED".to_vec(),
            )],
            passthrough_sequences: vec![0],
            close_after_render: true,
            close_sequence: Some(1),
            sustained: false,
        }),
    )
    .await
    .expect("finish attach exit");

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    let tail = exited
        .windows(b"DEQUEUED_FINAL_TAIL".len())
        .position(|window| window == b"DEQUEUED_FINAL_TAIL")
        .expect("the already-dequeued output must be delivered");
    let passthrough = exited
        .windows(b"\x1b_Gf=100;DEQUEUED\x1b\\".len())
        .position(|window| window == b"\x1b_Gf=100;DEQUEUED\x1b\\")
        .expect("the already-dequeued passthrough must be delivered");
    let banner = exited
        .windows(b"[exited]\r\n".len())
        .position(|window| window == b"[exited]\r\n")
        .expect("exit banner must be delivered");
    assert!(tail < banner);
    assert!(passthrough < banner);
}

#[tokio::test]
async fn exited_after_same_source_render_switch_does_not_duplicate_dequeued_output() {
    let session_name = SessionName::new("render-switch-exit-drain").expect("valid session name");
    let pane_output = pane_output_channel();
    let mut initial =
        test_attach_target_with_output(&session_name, b"BASE-0", None, pane_output.clone(), true);
    initial.pane_master = None;
    let mut current_target = open_attach_target(initial, true).expect("open initial target");

    let covered_sequence = pane_output
        .send_for_generation_with_passthroughs(
            None,
            b"COVERED_ONCE".to_vec(),
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                b"Gf=100;COVERED".to_vec(),
            )],
        )
        .expect("publish output covered by the refresh snapshot");
    let covered_item = current_target
        .pane_output
        .as_mut()
        .and_then(super::types::PaneOutputReceiver::try_recv)
        .expect("old receiver dequeues covered output before the switch");
    let pending_batch = collect_attach_output_batch(covered_item, None);

    let mut replacement = test_attach_target_with_output(
        &session_name,
        b"COVERED_ONCE",
        None,
        pane_output.clone(),
        true,
    );
    replacement.pane_master = None;
    assert_eq!(
        replacement.pane_output_start_sequence,
        covered_sequence + 1,
        "the replacement snapshot boundary follows the covered output"
    );
    let _ = pane_output.send_for_generation_with_passthroughs(
        None,
        b"AFTER_SNAPSHOT_ONCE".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;AFTER".to_vec(),
        )],
    );

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    control_tx
        .send(AttachControl::switch(replacement))
        .expect("queue same-source render refresh");
    control_tx
        .send(AttachControl::Exited)
        .expect("queue terminal exit");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();
    let control_backlog = AtomicUsize::new(0);
    let exit = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply switch and terminal exit");
    let PendingAttachAction::Exit(exit) = exit else {
        panic!("switch followed by Exited must terminate the attach");
    };
    let mut deferred_passthroughs = Vec::new();
    finish_pending_attach_exit_with_batch(
        exit.reason,
        &stream,
        &mut current_target,
        &mut deferred_passthroughs,
        pending_attach_exit_output_batch(
            exit.drop_pending_output,
            exit.snapshot_covered_output_before_sequence,
            pending_batch,
        ),
    )
    .await
    .expect("finish render-refresh exit");

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    for marker in [
        b"COVERED_ONCE".as_slice(),
        b"AFTER_SNAPSHOT_ONCE".as_slice(),
        b"\x1b_Gf=100;COVERED\x1b\\".as_slice(),
        b"\x1b_Gf=100;AFTER\x1b\\".as_slice(),
    ] {
        assert_eq!(
            exited
                .windows(marker.len())
                .filter(|bytes| *bytes == marker)
                .count(),
            1,
            "snapshot partition must deliver every output and passthrough exactly once: {marker:?}"
        );
    }
}

#[tokio::test]
async fn exited_after_non_coalesced_same_source_switches_forwards_passthroughs_once() {
    let session_name =
        SessionName::new("multi-render-switch-exit-drain").expect("valid session name");
    let pane_output = pane_output_channel();
    let mut clipboard_options = OptionStore::new();
    clipboard_options
        .set(
            ScopeSelector::Global,
            OptionName::SetClipboard,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("enable application clipboard passthrough");
    let outer_terminal = OuterTerminal::resolve(
        &clipboard_options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
    );
    let passthroughs = |suffix: &str, clipboard_payload: &[u8]| {
        vec![
            TerminalPassthrough::raw(0, 0, format!("RAW-{suffix}").into_bytes()),
            TerminalPassthrough::clipboard(clipboard_payload.to_vec()),
            TerminalPassthrough::kitty_graphics(
                0,
                0,
                format!("Gf=100;KITTY-{suffix}").into_bytes(),
            ),
            TerminalPassthrough::sixel(0, 0, format!("qSIXEL-{suffix}").into_bytes()),
        ]
    };

    let mut initial = test_attach_target_with_protocols(
        &session_name,
        b"BASE-0",
        None,
        pane_output.clone(),
        true,
        true,
    );
    initial.pane_master = None;
    initial.outer_terminal = outer_terminal.clone();
    let mut current_target = open_attach_target(initial, true).expect("open initial target");

    let sequence_0 = pane_output
        .send_for_generation_with_passthroughs(
            None,
            b"OUTPUT-0".to_vec(),
            passthroughs("0", b"\x1b]52;c;UDA=\x07"),
        )
        .expect("publish first output interval");
    let mut replacement_1 = test_attach_target_with_protocols(
        &session_name,
        b"SNAPSHOT-1",
        None,
        pane_output.clone(),
        true,
        true,
    );
    replacement_1.pane_master = None;
    replacement_1.outer_terminal = outer_terminal.clone();
    assert_eq!(
        replacement_1.pane_output_start_sequence,
        sequence_0 + 1,
        "first replacement must start after the first interval"
    );

    let sequence_1 = pane_output
        .send_for_generation_with_passthroughs(
            None,
            b"OUTPUT-1".to_vec(),
            passthroughs("1", b"\x1b]52;c;UDE=\x07"),
        )
        .expect("publish middle output interval");
    let mut replacement_2 = test_attach_target_with_protocols(
        &session_name,
        b"SNAPSHOT-2",
        None,
        pane_output.clone(),
        true,
        true,
    );
    replacement_2.pane_master = None;
    replacement_2.outer_terminal = outer_terminal;
    assert_eq!(
        replacement_2.pane_output_start_sequence,
        sequence_1 + 1,
        "second replacement must start after the middle interval"
    );

    let _sequence_2 = pane_output
        .send_for_generation_with_passthroughs(
            None,
            b"OUTPUT-2".to_vec(),
            passthroughs("2", b"\x1b]52;c;UDI=\x07"),
        )
        .expect("publish final output interval");

    let first_item = current_target
        .pane_output
        .as_mut()
        .and_then(super::types::PaneOutputReceiver::try_recv)
        .expect("old receiver dequeues the first interval");
    let pending_batch =
        collect_attach_output_batch(first_item, current_target.pane_output.as_mut());

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    control_tx
        .send(AttachControl::switch(replacement_1))
        .expect("queue first same-source render refresh");
    control_tx
        .send(AttachControl::Write(b"INTERLEAVED-CONTROL".to_vec()))
        .expect("separate the render refreshes so they cannot coalesce");
    control_tx
        .send(AttachControl::switch(replacement_2))
        .expect("queue second same-source render refresh");
    control_tx
        .send(AttachControl::Exited)
        .expect("queue terminal exit");

    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();
    let control_backlog = AtomicUsize::new(0);
    let exit = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
        None,
    )
    .await
    .expect("apply two refreshes and terminal exit");
    let PendingAttachAction::Exit(exit) = exit else {
        panic!("refreshes followed by Exited must terminate the attach");
    };
    let mut deferred_passthroughs = Vec::new();
    finish_pending_attach_exit_with_batch(
        exit.reason,
        &stream,
        &mut current_target,
        &mut deferred_passthroughs,
        pending_attach_exit_output_batch(
            exit.drop_pending_output,
            exit.snapshot_covered_output_before_sequence,
            pending_batch,
        ),
    )
    .await
    .expect("finish multi-refresh exit");

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    for marker in [
        b"RAW-0".as_slice(),
        b"RAW-1".as_slice(),
        b"RAW-2".as_slice(),
        b"\x1b]52;c;UDA=\x07".as_slice(),
        b"\x1b]52;c;UDE=\x07".as_slice(),
        b"\x1b]52;c;UDI=\x07".as_slice(),
        b"\x1b_Gf=100;KITTY-0\x1b\\".as_slice(),
        b"\x1b_Gf=100;KITTY-1\x1b\\".as_slice(),
        b"\x1b_Gf=100;KITTY-2\x1b\\".as_slice(),
        b"\x1bPqSIXEL-0\x1b\\".as_slice(),
        b"\x1bPqSIXEL-1\x1b\\".as_slice(),
        b"\x1bPqSIXEL-2\x1b\\".as_slice(),
    ] {
        assert_eq!(
            exited
                .windows(marker.len())
                .filter(|bytes| *bytes == marker)
                .count(),
            1,
            "every passthrough must be delivered exactly once: {marker:?}"
        );
    }
}

#[tokio::test]
async fn forward_attach_plain_refresh_does_not_clear_the_screen() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &session_name,
            b"BASE-1",
            None,
        )))
        .expect("send refreshed attach target");

    let refresh = read_attach_data_until(&mut peer, b"BASE-1").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
        !refresh_text.contains("\x1b[2J"),
        "plain pane-output refresh must not clear the whole terminal: {refresh_text:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_select_switch_preserves_fragmented_same_pane_input() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("refresh-pending-input").expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx.clone())
        .await;
    handler.start_attached_input_capture_for_test(&target).await;

    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let pane_output = pane_output_channel();
    let initial =
        test_attach_target_with_output(&session_name, b"BASE-0", None, pane_output.clone(), false);
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let attach_task = tokio::spawn(forward_attach(
        stream,
        initial,
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await,
        false,
    ));

    let _initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    let prefix = b"A\x1b_Gi=7";
    peer.write_all(
        &encode_attach_message(&AttachMessage::Data(prefix.to_vec()))
            .expect("encode fragmented Kitty prefix"),
    )
    .await
    .expect("write fragmented Kitty prefix");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if handler.attached_input_capture_for_test(&target).await == Some(b"A".to_vec()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("prefix should reach the attach loop before the switch");

    control_tx
        .send(AttachControl::switch(test_attach_target_with_output(
            &session_name,
            b"BASE-1",
            None,
            pane_output,
            false,
        )))
        .expect("send same-pane refresh through the select branch");
    let _refresh = read_attach_data_until(&mut peer, b"BASE-1").await;

    let suffix = b";OK\x1b\\";
    peer.write_all(
        &encode_attach_message(&AttachMessage::Data(suffix.to_vec()))
            .expect("encode fragmented Kitty suffix"),
    )
    .await
    .expect("write fragmented Kitty suffix");
    let expected = b"A\x1b_Gi=7;OK\x1b\\";
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if handler.attached_input_capture_for_test(&target).await == Some(expected.to_vec()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("same-pane refresh must preserve the fragmented Kitty APC");

    shutdown_tx.send(()).expect("request attach shutdown");
    assert!(attach_task.await.expect("attach task join").is_ok());
}

#[tokio::test]
async fn forward_attach_lock_boundary_discards_fragmented_input_before_unlock() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("lock-pending-input").expect("valid session name");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let escape_time = handler
        .handle(Request::SetOption(rmux_proto::SetOptionRequest {
            scope: rmux_proto::ScopeSelector::Global,
            option: rmux_proto::OptionName::EscapeTime,
            value: "30000".to_owned(),
            mode: rmux_proto::SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(escape_time, Response::SetOption(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx.clone())
        .await;
    handler.start_attached_input_capture_for_test(&target).await;

    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let initial = test_attach_target(&session_name, b"BASE-0", None);
    let expected_stop = initial.outer_terminal.attach_stop_sequence();
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let attach_task = tokio::spawn(forward_attach(
        stream,
        initial,
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await,
        false,
    ));

    let _initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    let prefix = b"A\x1b_Gi=7";
    peer.write_all(
        &encode_attach_message(&AttachMessage::Data(prefix.to_vec()))
            .expect("encode fragmented Kitty prefix"),
    )
    .await
    .expect("write fragmented Kitty prefix");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if handler.attached_input_capture_for_test(&target).await == Some(b"A".to_vec()) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fragment should reach the attach loop before lock");

    control_tx
        .send(AttachControl::LockShellCommand(AttachShellCommand::new(
            "lock-command".to_owned(),
            "/bin/sh".to_owned(),
            "/tmp".to_owned(),
        )))
        .expect("send lock control");
    let _stop = read_attach_data_until(&mut peer, &expected_stop).await;

    peer.write_all(&encode_attach_message(&AttachMessage::Unlock).expect("encode unlock"))
        .await
        .expect("write unlock");
    let suffix = b";OWNERSHIP\x1b\\";
    peer.write_all(
        &encode_attach_message(&AttachMessage::Data(suffix.to_vec()))
            .expect("encode post-unlock suffix"),
    )
    .await
    .expect("write post-unlock suffix");

    let captured = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let captured = handler
                .attached_input_capture_for_test(&target)
                .await
                .expect("input capture remains installed");
            if captured
                .windows(b"OWNERSHIP".len())
                .any(|window| window == b"OWNERSHIP")
            {
                break captured;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("post-unlock input should reach the pane");
    assert!(
        !captured
            .windows(b"Gi=7".len())
            .any(|window| window == b"Gi=7"),
        "pre-lock fragmented input must not cross the terminal ownership boundary: {captured:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    assert!(attach_task.await.expect("attach task join").is_ok());
}

#[tokio::test]
async fn forward_attach_preserves_persistent_overlay_across_stateful_switch_refreshes() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            b"MENU-OLD".to_vec(),
            0,
            1,
            7,
        )))
        .expect("send initial persistent overlay");
    let overlay = read_attach_data_until(&mut peer, b"MENU-OLD").await;
    assert!(
        String::from_utf8_lossy(&overlay).contains("MENU-OLD"),
        "persistent overlay should be visible before the refresh"
    );

    control_tx
        .send(AttachControl::AdvancePersistentOverlayState(8))
        .expect("send overlay state advance");
    control_tx
        .send(AttachControl::switch(test_attach_target(
            &session_name,
            b"BASE-1",
            Some(8),
        )))
        .expect("send refreshed attach target");

    let refresh = read_attach_data_until(&mut peer, b"MENU-OLD").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
            refresh_text.contains("BASE-1") && refresh_text.contains("MENU-OLD"),
            "stateful choose-tree refresh should compose the refreshed base and cached overlay in one render frame: {refresh_text:?}"
        );
    assert!(
            !refresh_text.contains("\x1b[2J"),
            "stateful choose-tree refresh must not clear to the base pane before the replacement overlay: {refresh_text:?}"
        );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_counts_coalesced_switches_before_persistent_overlay() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_render_only_attach_target(&session_name, b"BASE-0"),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::switch(test_render_only_attach_target(
            &session_name,
            b"BASE-1",
        )))
        .expect("send prompt close refresh");
    control_tx
        .send(AttachControl::switch(test_render_only_attach_target(
            &session_name,
            b"BASE-2",
        )))
        .expect("send session mutation refresh");
    control_tx
        .send(AttachControl::switch(
            test_render_only_attach_target_with_state(&session_name, b"BASE-3", Some(8)),
        ))
        .expect("send mode-tree switch");
    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            b"MENU-NEW".to_vec(),
            3,
            1,
            8,
        )))
        .expect("send mode-tree overlay");

    let refresh = read_attach_data_until(&mut peer, b"MENU-NEW").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
        refresh_text.contains("BASE-3") && refresh_text.contains("MENU-NEW"),
        "coalesced switch generation must still match the pending overlay: {refresh_text:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_emits_overlay_control_frames() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("alpha").expect("valid session name");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let split = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));
    let set_option = handler
        .handle(Request::SetOption(rmux_proto::SetOptionRequest {
            scope: rmux_proto::ScopeSelector::Session(session_name.clone()),
            option: rmux_proto::OptionName::DisplayPanesTime,
            value: "5000".to_owned(),
            mode: rmux_proto::SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_option, Response::SetOption(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let test_control_tx = control_tx.clone();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;

    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let pane_output = pane_output_channel();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name: session_name.clone(),
        pane_master: Some(pane_master),
        pane_output,
        pane_output_start_sequence,
        render_frame: Vec::new(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
    };

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext::current_for_test(handler, attach_pid).await;

    let attach_task = tokio::spawn(async move {
        forward_attach(
            stream,
            target,
            Vec::new(),
            shutdown_rx,
            control_rx,
            Arc::new(AtomicUsize::new(0)),
            closing,
            Arc::new(AtomicU64::new(0)),
            live_input,
            false,
        )
        .await
    });

    let mut frame_bytes = [0_u8; 4096];
    let mut decoder = AttachFrameDecoder::new();
    while let Ok(Ok(bytes_read)) =
        tokio::time::timeout(Duration::from_millis(25), peer.read(&mut frame_bytes)).await
    {
        if bytes_read == 0 {
            break;
        }
        decoder.push_bytes(&frame_bytes[..bytes_read]);
        while decoder
            .next_message()
            .expect("decode initial attach frame")
            .is_some()
        {}
    }

    let overlay_marker = b"\x1b[s\x1b[?25l";
    let overlay_frame =
        OverlayFrame::new(b"\x1b[s\x1b[?25lDISPLAY-PANES\x1b[0m\x1b[u".to_vec(), 0, 1);
    test_control_tx
        .send(AttachControl::Overlay(overlay_frame))
        .expect("send overlay control");
    let mut collected = Vec::new();
    let overlay_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = overlay_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let read_timeout = remaining.min(Duration::from_millis(250));
        let bytes_read = match tokio::time::timeout(read_timeout, peer.read(&mut frame_bytes)).await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(bytes_read)) => bytes_read,
            Ok(Err(error)) => panic!("read attach frame: {error}"),
            Err(_) => continue,
        };
        decoder.push_bytes(&frame_bytes[..bytes_read]);
        while let Some(message) = decoder.next_message().expect("decode attach frame") {
            match message {
                AttachMessage::Data(bytes) | AttachMessage::Render(bytes) => {
                    collected.extend_from_slice(&bytes)
                }
                _ => {}
            }
        }
        if collected
            .windows(overlay_marker.len())
            .any(|window| window == overlay_marker)
        {
            break;
        }
    }

    assert!(
        collected
            .windows(overlay_marker.len())
            .any(|window| window == overlay_marker),
        "overlay control should emit a frame, got: {:?}",
        String::from_utf8_lossy(&collected)
    );

    peer.shutdown().await.expect("close client peer");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}
