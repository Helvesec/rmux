use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;

use rmux_core::{input::InputEndType, PaneId, TerminalClipboardQuery};
use rmux_proto::{
    HookName, LinkWindowRequest, NewSessionRequest, NewWindowRequest, OptionName, PaneTarget,
    Request, RespawnPaneRequest, Response, ScopeSelector, SelectWindowRequest, SessionName,
    SetOptionMode, SetOptionRequest, SwitchClientRequest, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

use super::attach_support::{ActiveAttachIdentity, AttachRegistration, ClientFlags};
use super::RequestHandler;
use crate::clipboard_protocol::CLIPBOARD_QUERY_SEQUENCE;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::{AttachControl, PaneAlertEvent};
use crate::server_access::current_owner_uid;

struct PaneFixture {
    handler: RequestHandler,
    session: SessionName,
    target: PaneTarget,
    pane_id: PaneId,
    generation: u64,
}

#[derive(Clone, Copy)]
struct AttachSettings {
    flags: ClientFlags,
    render_stream: bool,
    can_write: bool,
}

impl Default for AttachSettings {
    fn default() -> Self {
        Self {
            flags: ClientFlags::default(),
            render_stream: false,
            can_write: true,
        }
    }
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_fixture(name: &str) -> PaneFixture {
    let handler = RequestHandler::new();
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    let target = PaneTarget::new(session.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    let (pane_id, generation) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&session)
            .and_then(|session| session.pane_id_in_window(0, 0))
            .expect("initial pane exists");
        let generation = state.pane_output_generation_for_target(&target, pane_id);
        state.start_pane_input_capture_for_test(&target);
        (pane_id, generation)
    };
    PaneFixture {
        handler,
        session,
        target,
        pane_id,
        generation,
    }
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

async fn set_global_option(handler: &RequestHandler, option: OptionName, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn enable_get_clipboard(handler: &RequestHandler, mode: &str) {
    set_global_option(handler, OptionName::SetClipboard, "on").await;
    set_global_option(handler, OptionName::GetClipboard, mode).await;
}

async fn register_attach(
    handler: &RequestHandler,
    attach_pid: u32,
    session: &SessionName,
    settings: AttachSettings,
) -> (u64, mpsc::UnboundedReceiver<AttachControl>) {
    register_attach_for_uid(handler, attach_pid, session, current_owner_uid(), settings).await
}

async fn register_attach_for_uid(
    handler: &RequestHandler,
    attach_pid: u32,
    session: &SessionName,
    uid: u32,
    settings: AttachSettings,
) -> (u64, mpsc::UnboundedReceiver<AttachControl>) {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach_with_access(
            attach_pid,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::default(),
                flags: settings.flags,
                render_stream: settings.render_stream,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: settings.can_write,
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
        )
        .await
        .expect("attach registration succeeds");
    (attach_id, control_rx)
}

async fn attach_identity(handler: &RequestHandler, attach_pid: u32) -> ActiveAttachIdentity {
    let active_attach = handler.active_attach.lock().await;
    active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach remains active")
        .identity(attach_pid)
}

async fn request_query(fixture: &PaneFixture, query: TerminalClipboardQuery) {
    fixture
        .handler
        .handle_pane_clipboard_queries(
            fixture.session.clone(),
            fixture.pane_id,
            Some(fixture.generation),
            vec![query],
        )
        .await;
}

async fn recv_clipboard_query(
    handler: &RequestHandler,
    attach_pid: u32,
    receiver: &mut mpsc::UnboundedReceiver<AttachControl>,
) {
    let backlog = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("attach remains active")
        .control_backlog
        .clone();
    loop {
        let control = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("clipboard query control is sent")
            .expect("attach control channel remains open");
        let received_units = control.received_backlog_units();
        let query = match control {
            AttachControl::Write(bytes) => Some(bytes),
            _ => None,
        };
        crate::pane_io::release_attach_control_backlog(&backlog, received_units);
        if let Some(bytes) = query {
            assert_eq!(bytes, CLIPBOARD_QUERY_SEQUENCE);
            return;
        }
    }
}

async fn captured_input(fixture: &PaneFixture) -> Vec<u8> {
    fixture
        .handler
        .state
        .lock()
        .await
        .pane_input_capture_for_test(&fixture.target)
        .expect("pane input capture remains installed")
}

async fn reset_capture(fixture: &PaneFixture) {
    fixture
        .handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&fixture.target);
}

async fn feed_clipboard_response(handler: &RequestHandler, attach_pid: u32, bytes: &[u8]) {
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending_input, bytes)
        .await
        .expect("attached OSC 52 response is consumed");
    assert!(pending_input.is_empty());
}

async fn begin_paused_both_response(
    fixture: &PaneFixture,
    attach_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> (
    Arc<super::clipboard_query_test_pause::ClipboardQueryTestPause>,
    tokio::task::JoinHandle<()>,
) {
    request_query(fixture, TerminalClipboardQuery::new("c", InputEndType::Bel)).await;
    recv_clipboard_query(&fixture.handler, attach_pid, control_rx).await;
    let commit_pause = fixture
        .handler
        .install_clipboard_response_commit_pause_for_test();
    let response_handler = fixture.handler.clone();
    let response = tokio::spawn(async move {
        feed_clipboard_response(
            &response_handler,
            attach_pid,
            b"\x1b]52;c;c3RhbGUtcmVzcG9uc2U=\x07",
        )
        .await;
    });
    timeout(Duration::from_secs(1), commit_pause.wait_until_reached())
        .await
        .expect("clipboard response reaches the pre-commit pause");
    (commit_pause, response)
}

async fn finish_paused_clipboard_response(
    pause: Arc<super::clipboard_query_test_pause::ClipboardQueryTestPause>,
    response: tokio::task::JoinHandle<()>,
) {
    pause.release();
    timeout(Duration::from_secs(1), response)
        .await
        .expect("clipboard response finishes after the commit pause")
        .expect("clipboard response task joins");
}

async fn seed_old_buffer(fixture: &PaneFixture) {
    fixture
        .handler
        .store_buffer(None, b"old-buffer".to_vec())
        .await
        .expect("old buffer stores");
}

async fn assert_old_buffer_is_unchanged(fixture: &PaneFixture) {
    let state = fixture.handler.state.lock().await;
    assert_eq!(state.buffers.len(), 1);
    let head = state.buffers.stack_head().expect("old buffer remains head");
    assert_eq!(state.buffers.get(head), Some(b"old-buffer".as_slice()));
}

#[tokio::test]
async fn buffer_mode_answers_detached_and_preserves_selector_and_terminator() {
    let fixture = create_fixture("clipboard-buffer").await;
    enable_get_clipboard(&fixture.handler, "buffer").await;
    fixture
        .handler
        .store_buffer(None, b"oracle-data".to_vec())
        .await
        .expect("buffer stores");

    request_query(
        &fixture,
        TerminalClipboardQuery::new("zzpc", InputEndType::St),
    )
    .await;
    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;p;b3JhY2xlLWRhdGE=\x1b\\"
    );

    {
        let mut state = fixture.handler.state.lock().await;
        state.buffers.delete(None).expect("buffer deletes");
        state.start_pane_input_capture_for_test(&fixture.target);
    }
    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn set_clipboard_external_and_get_clipboard_off_are_silent() {
    let fixture = create_fixture("clipboard-disabled").await;
    fixture
        .handler
        .store_buffer(None, b"secret".to_vec())
        .await
        .expect("buffer stores");

    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    assert!(captured_input(&fixture).await.is_empty());

    enable_get_clipboard(&fixture.handler, "off").await;
    reset_capture(&fixture).await;
    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn pane_alert_request_round_trip_uses_fixed_outer_query_without_changing_buffer() {
    let fixture = create_fixture("clipboard-request").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    fixture
        .handler
        .store_buffer(None, b"old-buffer".to_vec())
        .await
        .expect("buffer stores");
    let (_, mut control_rx) = register_attach(
        &fixture.handler,
        101,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;

    fixture.handler.pane_alert_callback()(PaneAlertEvent {
        session_name: fixture.session.clone(),
        pane_id: fixture.pane_id,
        bell_count: 0,
        title_changed: false,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: vec![TerminalClipboardQuery::new("0c", InputEndType::Bel)],
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: Some(fixture.generation),
    });
    recv_clipboard_query(&fixture.handler, 101, &mut control_rx).await;
    feed_clipboard_response(&fixture.handler, 101, b"\x1b]52;c;b3V0ZXItZGF0YQ==\x1b\\").await;

    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;c;b3V0ZXItZGF0YQ==\x07"
    );
    let state = fixture.handler.state.lock().await;
    let head = state.buffers.stack_head().expect("old buffer remains");
    assert_eq!(state.buffers.get(head), Some(b"old-buffer".as_slice()));
}

#[tokio::test]
async fn both_mode_stores_response_with_buffer_limit_without_pane_set_clipboard_hook() {
    let fixture = create_fixture("clipboard-both").await;
    enable_get_clipboard(&fixture.handler, "both").await;
    set_global_option(&fixture.handler, OptionName::BufferLimit, "1").await;
    fixture
        .handler
        .store_buffer(None, b"old-buffer".to_vec())
        .await
        .expect("old buffer stores");
    let mut lifecycle = fixture.handler.subscribe_lifecycle_events();
    let (_, mut control_rx) = register_attach(
        &fixture.handler,
        102,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;

    request_query(&fixture, TerminalClipboardQuery::new("c", InputEndType::St)).await;
    recv_clipboard_query(&fixture.handler, 102, &mut control_rx).await;
    let store_pause = fixture
        .handler
        .install_clipboard_response_store_pause_for_test();
    let response_handler = fixture.handler.clone();
    let response = tokio::spawn(async move {
        feed_clipboard_response(&response_handler, 102, b"\x1b]52;c;bmV3LWRhdGE=\x07").await;
    });
    timeout(Duration::from_secs(1), store_pause.wait_until_reached())
        .await
        .expect("both mode stores before pane response");

    assert!(
        captured_input(&fixture).await.is_empty(),
        "the pane response must remain blocked until after buffer storage"
    );
    {
        let state = fixture.handler.state.lock().await;
        let head = state.buffers.stack_head().expect("response buffer is head");
        assert_eq!(state.buffers.get(head), Some(b"new-data".as_slice()));
    }
    store_pause.release();
    response.await.expect("response task joins");

    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;c;bmV3LWRhdGE=\x1b\\"
    );
    {
        let state = fixture.handler.state.lock().await;
        assert_eq!(state.buffers.len(), 1);
        let head = state.buffers.stack_head().expect("response buffer is head");
        assert_eq!(state.buffers.get(head), Some(b"new-data".as_slice()));
    }
    let mut hook_names = Vec::new();
    while let Ok(event) = lifecycle.try_recv() {
        hook_names.push(event.hook_name);
    }
    assert!(hook_names.contains(&HookName::PasteBufferDeleted));
    assert!(hook_names.contains(&HookName::PasteBufferChanged));
    assert!(!hook_names.contains(&HookName::PaneSetClipboard));
}

#[tokio::test]
async fn both_mode_downgrade_before_commit_preserves_the_existing_buffer() {
    let fixture = create_fixture("clipboard-both-downgrade").await;
    enable_get_clipboard(&fixture.handler, "both").await;
    seed_old_buffer(&fixture).await;
    let uid = current_owner_uid().wrapping_add(41_001);
    let (_, mut control_rx) = register_attach_for_uid(
        &fixture.handler,
        601,
        &fixture.session,
        uid,
        AttachSettings::default(),
    )
    .await;
    let (pause, response) = begin_paused_both_response(&fixture, 601, &mut control_rx).await;

    fixture.handler.update_live_access_mode(uid, false).await;
    finish_paused_clipboard_response(pause, response).await;

    assert_old_buffer_is_unchanged(&fixture).await;
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn both_mode_revocation_before_commit_preserves_the_existing_buffer() {
    let fixture = create_fixture("clipboard-both-revoke").await;
    enable_get_clipboard(&fixture.handler, "both").await;
    seed_old_buffer(&fixture).await;
    let uid = current_owner_uid().wrapping_add(41_002);
    let (_, mut control_rx) = register_attach_for_uid(
        &fixture.handler,
        602,
        &fixture.session,
        uid,
        AttachSettings::default(),
    )
    .await;
    let (pause, response) = begin_paused_both_response(&fixture, 602, &mut control_rx).await;

    fixture.handler.disconnect_clients_by_uid(uid).await;
    finish_paused_clipboard_response(pause, response).await;

    assert_old_buffer_is_unchanged(&fixture).await;
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn both_mode_same_pid_attach_replacement_before_commit_preserves_the_existing_buffer() {
    let fixture = create_fixture("clipboard-both-attach-replacement").await;
    enable_get_clipboard(&fixture.handler, "both").await;
    seed_old_buffer(&fixture).await;
    let attach_pid = 603;
    let (attach_id, mut control_rx) = register_attach(
        &fixture.handler,
        attach_pid,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    let (pause, response) = begin_paused_both_response(&fixture, attach_pid, &mut control_rx).await;

    fixture.handler.finish_attach(attach_pid, attach_id).await;
    let (_, mut replacement_rx) = register_attach(
        &fixture.handler,
        attach_pid,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    finish_paused_clipboard_response(pause, response).await;

    assert_old_buffer_is_unchanged(&fixture).await;
    assert!(captured_input(&fixture).await.is_empty());
    assert!(
        replacement_rx.try_recv().is_err(),
        "the stale response must not target the replacement attach"
    );
}

#[tokio::test]
async fn both_mode_pane_respawn_before_commit_preserves_the_existing_buffer() {
    let fixture = create_fixture("clipboard-both-pane-respawn").await;
    enable_get_clipboard(&fixture.handler, "both").await;
    seed_old_buffer(&fixture).await;
    let (_, mut control_rx) = register_attach(
        &fixture.handler,
        604,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    let (pause, response) = begin_paused_both_response(&fixture, 604, &mut control_rx).await;

    let respawn = fixture
        .handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: fixture.target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(matches!(respawn, Response::RespawnPane(_)), "{respawn:?}");
    finish_paused_clipboard_response(pause, response).await;

    assert_old_buffer_is_unchanged(&fixture).await;
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn timed_out_attach_generation_cannot_consume_healthy_fallback_queries() {
    let fixture = create_fixture("clipboard-timeout-generation").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    seed_old_buffer(&fixture).await;
    let (_, mut healthy_rx) = register_attach(
        &fixture.handler,
        605,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    let (timed_out_attach_id, mut timed_out_rx) = register_attach(
        &fixture.handler,
        606,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    let timed_out_identity = attach_identity(&fixture.handler, 606).await;

    tokio::time::pause();
    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    recv_clipboard_query(&fixture.handler, 606, &mut timed_out_rx).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(250)).await;

    enable_get_clipboard(&fixture.handler, "both").await;
    request_query(&fixture, TerminalClipboardQuery::new("q", InputEndType::St)).await;
    recv_clipboard_query(&fixture.handler, 606, &mut timed_out_rx).await;
    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 2);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(251)).await;
    tokio::task::yield_now().await;

    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 0);
    {
        let active_attach = fixture.handler.active_attach.lock().await;
        assert!(active_attach.by_pid[&606].clipboard_queries_desynchronized);
        assert!(!active_attach.by_pid[&605].clipboard_queries_desynchronized);
    }

    request_query(&fixture, TerminalClipboardQuery::new("q", InputEndType::St)).await;
    recv_clipboard_query(&fixture.handler, 605, &mut healthy_rx).await;
    assert!(
        timed_out_rx.try_recv().is_err(),
        "a desynchronized attach must not receive another clipboard query"
    );
    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 1);

    assert!(!fixture
        .handler
        .handle_attached_clipboard_response(
            timed_out_identity,
            Some(b'c'),
            b"late-expired".to_vec(),
        )
        .await
        .expect("the late expired response is consumed safely"));
    feed_clipboard_response(
        &fixture.handler,
        606,
        b"\x1b]52;q;bGF0ZS1jYW5jZWxsZWQ=\x1b\\",
    )
    .await;
    assert_eq!(
        fixture.handler.pending_clipboard_query_count_for_test(),
        1,
        "the stale response must not consume the healthy attach's query"
    );
    assert_old_buffer_is_unchanged(&fixture).await;
    assert!(captured_input(&fixture).await.is_empty());

    feed_clipboard_response(&fixture.handler, 605, b"\x1b]52;p;aGVhbHRoeQ==\x07").await;
    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;p;aGVhbHRoeQ==\x1b\\"
    );
    {
        let state = fixture.handler.state.lock().await;
        let head = state
            .buffers
            .stack_head()
            .expect("healthy response is stored");
        assert_eq!(state.buffers.get(head), Some(b"healthy".as_slice()));
    }

    reset_capture(&fixture).await;
    enable_get_clipboard(&fixture.handler, "request").await;
    let (replacement_attach_id, mut replacement_rx) = register_attach(
        &fixture.handler,
        606,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    assert_ne!(replacement_attach_id, timed_out_attach_id);
    {
        let active_attach = fixture.handler.active_attach.lock().await;
        assert!(!active_attach.by_pid[&606].clipboard_queries_desynchronized);
    }

    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    recv_clipboard_query(&fixture.handler, 606, &mut replacement_rx).await;
    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 1);
    assert!(!fixture
        .handler
        .handle_attached_clipboard_response(
            timed_out_identity,
            Some(b'c'),
            b"late-old-generation".to_vec(),
        )
        .await
        .expect("the replaced generation response is consumed safely"));
    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 1);

    feed_clipboard_response(&fixture.handler, 606, b"\x1b]52;c;bmV3LWdlbmVyYXRpb24=\x07").await;
    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;c;bmV3LWdlbmVyYXRpb24=\x07"
    );
}

#[tokio::test]
async fn pane_alert_callbacks_keep_clipboard_queries_in_publication_order() {
    let fixture = create_fixture("clipboard-query-order").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    let (_, mut control_rx) = register_attach(
        &fixture.handler,
        103,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    let drain_pause = fixture.handler.install_pane_query_drain_pause_for_test();
    let callback = fixture.handler.pane_alert_callback();
    let event = |query| PaneAlertEvent {
        session_name: fixture.session.clone(),
        pane_id: fixture.pane_id,
        bell_count: 0,
        title_changed: false,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: vec![query],
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: Some(fixture.generation),
    };

    callback(event(TerminalClipboardQuery::new("p", InputEndType::Bel)));
    timeout(Duration::from_secs(1), drain_pause.wait_until_reached())
        .await
        .expect("first query worker reaches deterministic pause");
    callback(event(TerminalClipboardQuery::new("q", InputEndType::St)));
    tokio::task::yield_now().await;
    assert!(
        control_rx.try_recv().is_err(),
        "a second callback must not bypass the paused FIFO worker"
    );
    drain_pause.release();

    recv_clipboard_query(&fixture.handler, 103, &mut control_rx).await;
    recv_clipboard_query(&fixture.handler, 103, &mut control_rx).await;
    feed_clipboard_response(
        &fixture.handler,
        103,
        b"\x1b]52;c;Zmlyc3Q=\x07\x1b]52;p;c2Vjb25k\x1b\\",
    )
    .await;
    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;c;Zmlyc3Q=\x07\x1b]52;p;c2Vjb25k\x1b\\"
    );
}

#[tokio::test]
async fn pane_alert_clipboard_query_queue_is_bounded_while_the_worker_is_busy() {
    let fixture = create_fixture("clipboard-query-queue-bound").await;
    let drain_pause = fixture.handler.install_pane_query_drain_pause_for_test();
    let callback = fixture.handler.pane_alert_callback();
    let event = |queries| PaneAlertEvent {
        session_name: fixture.session.clone(),
        pane_id: fixture.pane_id,
        bell_count: 0,
        title_changed: false,
        title_change: None,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: queries,
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: Some(fixture.generation),
    };

    callback(event(vec![TerminalClipboardQuery::new(
        "p",
        InputEndType::Bel,
    )]));
    timeout(Duration::from_secs(1), drain_pause.wait_until_reached())
        .await
        .expect("query worker reaches deterministic pause");
    callback(event(
        (0..65)
            .map(|_| TerminalClipboardQuery::new("q", InputEndType::St))
            .collect(),
    ));
    assert_eq!(fixture.handler.queued_clipboard_query_count_for_test(), 64);

    drain_pause.release();
    timeout(Duration::from_secs(1), async {
        while fixture.handler.queued_clipboard_query_count_for_test() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded query queue drains");
}

#[tokio::test]
async fn request_prefers_latest_eligible_client_and_excludes_ineligible_attaches() {
    let fixture = create_fixture("clipboard-client-choice").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    let unrelated = create_session(&fixture.handler, "clipboard-unrelated").await;
    let (_, mut latest_rx) = register_attach(
        &fixture.handler,
        201,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    let (_, mut read_only_rx) = register_attach(
        &fixture.handler,
        202,
        &fixture.session,
        AttachSettings {
            flags: ClientFlags::READONLY,
            ..AttachSettings::default()
        },
    )
    .await;
    let (_, mut render_rx) = register_attach(
        &fixture.handler,
        203,
        &fixture.session,
        AttachSettings {
            render_stream: true,
            ..AttachSettings::default()
        },
    )
    .await;
    let (_, mut no_write_rx) = register_attach(
        &fixture.handler,
        204,
        &fixture.session,
        AttachSettings {
            can_write: false,
            ..AttachSettings::default()
        },
    )
    .await;
    let (_, mut unrelated_rx) =
        register_attach(&fixture.handler, 205, &unrelated, AttachSettings::default()).await;
    let (_, mut suspended_rx) = register_attach(
        &fixture.handler,
        206,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    fixture
        .handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&206)
        .expect("suspended attach exists")
        .suspended = true;
    let (_, mut other_eligible_rx) = register_attach(
        &fixture.handler,
        207,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    feed_clipboard_response(&fixture.handler, 201, b"\x1b[?62;52;c").await;

    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    recv_clipboard_query(&fixture.handler, 201, &mut latest_rx).await;
    for receiver in [
        &mut read_only_rx,
        &mut render_rx,
        &mut no_write_rx,
        &mut unrelated_rx,
        &mut suspended_rx,
        &mut other_eligible_rx,
    ] {
        assert!(
            receiver.try_recv().is_err(),
            "ineligible or older client selected"
        );
    }
}

#[tokio::test]
async fn inactive_window_stays_eligible_but_session_switch_and_replacement_drop_responses() {
    let fixture = create_fixture("clipboard-attach-races").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    let unrelated = create_session(&fixture.handler, "clipboard-switched-away").await;
    let response = fixture
        .handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: fixture.session.clone(),
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
    let Response::NewWindow(new_window) = response else {
        panic!("expected new-window response, got {response:?}");
    };
    let (attach_id, mut control_rx) = register_attach(
        &fixture.handler,
        301,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;

    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    recv_clipboard_query(&fixture.handler, 301, &mut control_rx).await;
    assert!(matches!(
        fixture
            .handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: new_window.target.clone(),
            }))
            .await,
        Response::SelectWindow(_)
    ));
    feed_clipboard_response(&fixture.handler, 301, b"\x1b]52;c;c3dpdGNoZWQ=\x07").await;
    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;c;c3dpdGNoZWQ=\x07"
    );
    reset_capture(&fixture).await;

    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    recv_clipboard_query(&fixture.handler, 301, &mut control_rx).await;
    let switched = fixture
        .handler
        .dispatch(
            301,
            Request::SwitchClient(SwitchClientRequest { target: unrelated }),
        )
        .await;
    assert!(matches!(switched.response, Response::SwitchClient(_)));
    feed_clipboard_response(&fixture.handler, 301, b"\x1b]52;c;b3RoZXItc2Vzc2lvbg==\x07").await;
    assert!(captured_input(&fixture).await.is_empty());
    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 0);

    let switched = fixture
        .handler
        .dispatch(
            301,
            Request::SwitchClient(SwitchClientRequest {
                target: fixture.session.clone(),
            }),
        )
        .await;
    assert!(matches!(switched.response, Response::SwitchClient(_)));
    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    recv_clipboard_query(&fixture.handler, 301, &mut control_rx).await;
    fixture.handler.finish_attach(301, attach_id).await;
    let (_, mut replacement_rx) = register_attach(
        &fixture.handler,
        301,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    feed_clipboard_response(&fixture.handler, 301, b"\x1b]52;c;cmV1c2Vk\x07").await;
    assert!(replacement_rx.try_recv().is_err());
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn linked_inactive_window_is_eligible_by_stable_window_identity() {
    let fixture = create_fixture("clipboard-linked-source").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    let linked = create_session(&fixture.handler, "clipboard-linked-client").await;
    let response = fixture
        .handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(fixture.session.clone(), 0),
            target: WindowTarget::with_window(linked.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    {
        let state = fixture.handler.state.lock().await;
        let source_window = state
            .sessions
            .session(&fixture.session)
            .and_then(|session| session.window_at(0))
            .expect("source window exists");
        let linked_session = state
            .sessions
            .session(&linked)
            .expect("linked session exists");
        let linked_window = linked_session.window_at(1).expect("linked window exists");
        assert_eq!(linked_window.id(), source_window.id());
        assert_ne!(linked_session.active_window_index(), 1);
    }
    let (_, mut control_rx) =
        register_attach(&fixture.handler, 303, &linked, AttachSettings::default()).await;

    request_query(&fixture, TerminalClipboardQuery::new("p", InputEndType::St)).await;
    recv_clipboard_query(&fixture.handler, 303, &mut control_rx).await;
    feed_clipboard_response(&fixture.handler, 303, b"\x1b]52;c;bGlua2Vk\x07").await;
    assert_eq!(captured_input(&fixture).await, b"\x1b]52;c;bGlua2Vk\x1b\\");
}

#[tokio::test]
async fn pane_generation_change_drops_late_response() {
    let fixture = create_fixture("clipboard-pane-race").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    let (_, mut control_rx) = register_attach(
        &fixture.handler,
        302,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    request_query(
        &fixture,
        TerminalClipboardQuery::new("c", InputEndType::Bel),
    )
    .await;
    recv_clipboard_query(&fixture.handler, 302, &mut control_rx).await;

    let response = fixture
        .handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: fixture.target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    feed_clipboard_response(&fixture.handler, 302, b"\x1b]52;c;c3RhbGU=\x07").await;
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn pending_queries_are_fifo_bounded_and_expire() {
    let fixture = create_fixture("clipboard-pending").await;
    enable_get_clipboard(&fixture.handler, "request").await;
    let (_, mut control_rx) = register_attach(
        &fixture.handler,
        401,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;

    request_query(
        &fixture,
        TerminalClipboardQuery::new("p", InputEndType::Bel),
    )
    .await;
    request_query(&fixture, TerminalClipboardQuery::new("q", InputEndType::St)).await;
    recv_clipboard_query(&fixture.handler, 401, &mut control_rx).await;
    recv_clipboard_query(&fixture.handler, 401, &mut control_rx).await;
    feed_clipboard_response(
        &fixture.handler,
        401,
        b"\x1b]52;c;Zmlyc3Q=\x07\x1b]52;p;c2Vjb25k\x1b\\",
    )
    .await;
    assert_eq!(
        captured_input(&fixture).await,
        b"\x1b]52;c;Zmlyc3Q=\x07\x1b]52;p;c2Vjb25k\x1b\\"
    );

    reset_capture(&fixture).await;
    let queries = (0..65)
        .map(|_| TerminalClipboardQuery::new("c", InputEndType::Bel))
        .collect();
    fixture
        .handler
        .handle_pane_clipboard_queries(
            fixture.session.clone(),
            fixture.pane_id,
            Some(fixture.generation),
            queries,
        )
        .await;
    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 64);
    for _ in 0..64 {
        recv_clipboard_query(&fixture.handler, 401, &mut control_rx).await;
    }
    assert!(
        control_rx.try_recv().is_err(),
        "the 65th query must be rejected"
    );

    tokio::time::sleep(Duration::from_millis(550)).await;
    assert_eq!(fixture.handler.pending_clipboard_query_count_for_test(), 0);
    feed_clipboard_response(&fixture.handler, 401, b"\x1b]52;c;bGF0ZQ==\x07").await;
    assert!(captured_input(&fixture).await.is_empty());
}

#[tokio::test]
async fn unsolicited_clipboard_response_is_consumed_without_reaching_the_pane() {
    let fixture = create_fixture("clipboard-unsolicited").await;
    let (_, _) = register_attach(
        &fixture.handler,
        501,
        &fixture.session,
        AttachSettings::default(),
    )
    .await;
    let identity = attach_identity(&fixture.handler, 501).await;

    assert!(!fixture
        .handler
        .handle_attached_clipboard_response(identity, None, b"unsolicited".to_vec())
        .await
        .expect("unsolicited response is rejected"));
    feed_clipboard_response(&fixture.handler, 501, b"\x1b]52;c;dW5zb2xpY2l0ZWQ=\x07").await;
    assert!(captured_input(&fixture).await.is_empty());
}
