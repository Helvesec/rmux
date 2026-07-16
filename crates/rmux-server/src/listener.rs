use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmux_ipc::{wait_for_peer_close, LocalListener, LocalStream, PeerIdentity};
use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, Request, Response, WaitForMode,
    CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2, CAPABILITY_SDK_WAITS_ARMED,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinError, JoinSet};
use tracing::{debug, warn};

use crate::control::{self, ControlLifecycle, ControlServerEvent, ControlUpgradeInput};
use crate::daemon::ShutdownHandle;
use crate::handler::{
    attach_support::AttachRegistration, with_session_lease_create_addressing,
    ControlClientIdentity, ControlRegistration, DetachedRequestGuard, PreparedSdkWait,
    RequestHandler, SessionLeaseCreateAddressing,
};
use crate::listener_options::ServeOptions;
use crate::listener_signals::handle_server_signal;
use crate::listener_signals::poll_server_signal;
use crate::listener_signals::wait_server_signal;
use crate::pane_io;
use crate::server_access::apply_access_policy;
use crate::socket_cleanup::SocketCleanup;

mod legacy_shutdown;

use legacy_shutdown::{
    encode_legacy_kill_server_response, inspect_legacy_kill_server_frame, LegacyKillServerFrame,
    PublishedLegacyWireVersion,
};

const CONNECTION_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);
const LIFECYCLE_HOOK_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const DETACHED_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Accept loop: spawns a per-connection task for each incoming client.
pub(crate) async fn serve(
    mut listener: LocalListener,
    socket_path: PathBuf,
    shutdown_handle: ShutdownHandle,
    mut shutdown: oneshot::Receiver<()>,
    options: ServeOptions,
) -> io::Result<()> {
    #[cfg(unix)]
    let mut cleanup_on_drop = SocketCleanup::new(socket_path.clone(), options.socket_identity);
    #[cfg(windows)]
    let mut cleanup_on_drop = SocketCleanup::new(socket_path.clone());
    let server_signals = options.server_signals;
    #[cfg(all(any(unix, windows), feature = "web"))]
    let web_required = options.web_required;
    #[cfg(all(any(unix, windows), feature = "web"))]
    let handler = Arc::new(
        RequestHandler::with_owner_uid_subscription_limits_and_web_settings(
            options.owner_uid,
            options.subscription_limits,
            crate::web::WebShareSettings::from_options_with_port_explicit(
                options.web_port,
                options.web_frontend,
                options.web_port_explicit,
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?,
        ),
    );
    #[cfg(not(all(any(unix, windows), feature = "web")))]
    let handler = Arc::new(RequestHandler::with_owner_uid_and_subscription_limits(
        options.owner_uid,
        options.subscription_limits,
    ));
    handler.install_shutdown_handle(shutdown_handle.clone());
    handler.set_socket_path(&socket_path);
    let lifecycle_events = handler
        .take_lifecycle_dispatch_receiver()
        .ok_or_else(|| io::Error::other("lifecycle dispatch receiver already active"))?;
    #[cfg(all(any(unix, windows), feature = "web"))]
    if web_required {
        handler
            .ensure_web_share_listener_running()
            .await
            .map_err(|error| io::Error::new(io::ErrorKind::AddrNotAvailable, error.to_string()))?;
    }
    let (connection_shutdown, connection_shutdown_rx) = watch::channel(());
    let (hook_shutdown, hook_shutdown_rx) = oneshot::channel();
    let mut connection_tasks = JoinSet::new();
    let hook_handler = Arc::clone(&handler);
    let mut hook_task = tokio::spawn(async move {
        hook_handler
            .consume_lifecycle_hooks(lifecycle_events, hook_shutdown_rx)
            .await;
    });
    let startup_guard = handler.start_config_loading();
    let startup_handler = Arc::clone(&handler);
    let startup_config = options.config_load;
    let startup_task = tokio::spawn(async move {
        startup_handler
            .load_startup_config_with_guard(startup_config, startup_guard)
            .await;
    });

    loop {
        drain_finished_connection_tasks(&mut connection_tasks);

        tokio::select! {
            result = listener.accept() => {
                let (stream, requester) = match result {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        warn!("client accept failed; keeping server accept loop alive: {error}");
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        continue;
                    }
                };
                let handler = Arc::clone(&handler);
                let connection_shutdown = connection_shutdown_rx.clone();
                let shutdown_handle = shutdown_handle.clone();

                connection_tasks.spawn(async move {
                    let connection_id = handler.allocate_connection_id();
                    run_connection_with_cleanup(
                        stream,
                        requester,
                        handler,
                        connection_id,
                        connection_shutdown,
                        shutdown_handle,
                    )
                    .await
                });
            }
            _ = &mut shutdown => {
                debug!("shutdown requested");
                break;
            }
            result = wait_server_signal(&server_signals), if server_signals.is_some() => {
                if let Err(error) = result {
                    warn!("server signal wake failed; keeping server accept loop alive: {error}");
                }
                while let Some(signal) = poll_server_signal(&server_signals) {
                    handle_server_signal(
                        Some(signal),
                        &shutdown_handle,
                        &handler,
                        &socket_path,
                        &mut listener,
                        &mut cleanup_on_drop,
                    ).await;
                }
            }
        }
    }

    drop(connection_shutdown);
    startup_task.abort();
    match startup_task.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => warn!("startup config task failed: {error}"),
    }

    drain_connection_tasks_for_shutdown(&mut connection_tasks).await;
    handler.shutdown_wait_for();
    let _ = hook_shutdown.send(());
    // Keep shell registration open while already accepted lifecycle hooks drain. The
    // shared deadline also bounds older background jobs before the final tree cleanup.
    let hook_result = tokio::time::timeout(LIFECYCLE_HOOK_SHUTDOWN_GRACE, &mut hook_task).await;
    handler.shutdown_shell_processes();
    match hook_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!("lifecycle hook task failed: {error}"),
        Err(_) => {
            warn!("aborting lifecycle hooks that did not drain during daemon shutdown");
            hook_task.abort();
            match hook_task.await {
                Err(error) if error.is_cancelled() => {}
                Err(error) => warn!("lifecycle hook task failed after abort: {error}"),
                Ok(()) => {}
            }
        }
    }

    // Keep the old endpoint reserved until every accepted lifecycle hook has either
    // completed or been cancelled. Releasing it earlier lets an old hook reconnect
    // to a new daemon generation through its inherited RMUX/TMUX environment.
    drop(listener);
    cleanup_on_drop.cleanup_now();

    Ok(())
}

/// Read-dispatch-write loop for a single client connection.
async fn serve_connection(
    stream: LocalStream,
    requester: PeerIdentity,
    handler: Arc<RequestHandler>,
    connection_id: u64,
    mut shutdown: watch::Receiver<()>,
    shutdown_handle: ShutdownHandle,
) -> io::Result<()> {
    let Some(_) = handler.access_mode_for_peer(&requester) else {
        let mut conn = Connection::new(stream);
        conn.write_response(&Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("access not allowed".to_owned()),
        }))
        .await?;
        return Ok(());
    };
    let mut conn = Connection::new(stream);
    let detached_connection_guard = handler.begin_detached_connection(connection_id);
    let mut sdk_wait_armed_ack_enabled = false;
    let mut session_lease_by_id_enabled = false;

    loop {
        tokio::select! {
            request = conn.read_request() => {
                let Some(incoming_request) = request? else {
                    return Ok(());
                };
                let legacy_kill_server_wire = incoming_request.legacy_kill_server_wire;
                let request = incoming_request.request;
                let Some(access_mode) = handler.access_mode_for_peer(&requester) else {
                    conn.write_response(&Response::Error(ErrorResponse {
                        error: rmux_proto::RmuxError::Server("access not allowed".to_owned()),
                    }))
                    .await?;
                    continue;
                };
                let can_write = access_mode.can_write();
                let request = match apply_access_policy(request, can_write) {
                    Ok(request) => request,
                    Err(error) => {
                        conn.write_response(&Response::Error(ErrorResponse { error })).await?;
                        continue;
                    }
                };
                let _requester_access_guard =
                    handler.begin_detached_requester_access(requester.pid, can_write);
                let mut detached_request_guard = request_counts_as_detached_activity(&request)
                    .then(|| handler.begin_detached_request());

                if request_enables_sdk_wait_armed_ack(&request) {
                    sdk_wait_armed_ack_enabled = true;
                }
                let enables_session_lease_by_id =
                    request_enables_session_lease_by_id(&request);
                let session_lease_create_addressing = if session_lease_by_id_enabled {
                    SessionLeaseCreateAddressing::StableId
                } else {
                    SessionLeaseCreateAddressing::Nominal
                };
                let cancel_on_peer_disconnect = request_cancels_on_peer_disconnect(&request);
                debug!("dispatching {}", request.command_name());
                #[cfg(feature = "web")]
                let mut undelivered_web_share;
                let mut outcome = match request {
                    Request::SdkWaitForOutput(request) => {
                        let prepared = handler
                            .prepare_sdk_wait_for_output(connection_id, request)
                            .await;
                        if write_prepared_sdk_wait(
                            &mut conn,
                            prepared,
                            &mut shutdown,
                            &handler,
                            connection_id,
                            &mut detached_request_guard,
                            sdk_wait_armed_ack_enabled,
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        continue;
                    }
                    Request::SdkWaitForOutputRef(request) => {
                        let prepared = handler
                            .prepare_sdk_wait_for_output_ref(connection_id, request)
                            .await;
                        if write_prepared_sdk_wait(
                            &mut conn,
                            prepared,
                            &mut shutdown,
                            &handler,
                            connection_id,
                            &mut detached_request_guard,
                            sdk_wait_armed_ack_enabled,
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        continue;
                    }
                    request => {
                        #[cfg(feature = "web")]
                        let dispatch = handler.dispatch_for_connection_with_web_share_guard(
                            requester.pid,
                            connection_id,
                            request,
                        );
                        #[cfg(not(feature = "web"))]
                        let dispatch = handler.dispatch_for_connection(
                            requester.pid,
                            connection_id,
                            request,
                        );
                        tokio::select! {
                            outcome = with_session_lease_create_addressing(
                                session_lease_create_addressing,
                                dispatch,
                            ) => {
                                #[cfg(feature = "web")]
                                {
                                    undelivered_web_share = outcome.1;
                                    outcome.0
                                }
                                #[cfg(not(feature = "web"))]
                                outcome
                            },
                            result = shutdown.changed() => {
                                if result.is_ok() {
                                    debug!("closing client connection during shutdown");
                                }
                                return Ok(());
                            }
                            result = wait_for_peer_close(&conn.stream), if cancel_on_peer_disconnect => {
                                result?;
                                debug!("closing client connection after peer disconnect");
                                return Ok(());
                            }
                        }
                    }
                };
                if enables_session_lease_by_id
                    && matches!(&outcome.response, Response::Handshake(_))
                {
                    session_lease_by_id_enabled = true;
                }
                // An attach registration is removed as soon as its last
                // session closes, but its forwarder still has to deliver the
                // terminal exit frame. Bridge that transition before any
                // further await can let exit-empty shut the daemon down.
                let attach_forwarder_guard = outcome
                    .attach
                    .is_some()
                    .then(|| handler.begin_attach_forwarder());
                let pending_control = if let Some(control_upgrade) = outcome.control.take() {
                    let initial_command_count = control_upgrade.initial_command_count as usize;
                    if let Err(error) =
                        control::validate_initial_control_command_count(initial_command_count)
                    {
                        conn.write_response(&Response::Error(ErrorResponse { error }))
                            .await?;
                        drop(detached_request_guard.take());
                        continue;
                    }
                    let control_mode = control_upgrade.mode;
                    let (server_event_tx, server_event_rx) =
                        tokio::sync::mpsc::channel::<ControlServerEvent>(
                            control::CONTROL_SERVER_EVENT_CAPACITY,
                        );
                    let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let control_id = match handler
                        .register_control_with_access(
                            requester.pid,
                            control_upgrade,
                            ControlRegistration {
                                event_tx: server_event_tx,
                                closing: closing.clone(),
                                uid: requester.uid,
                                user: requester.user.clone(),
                                can_write,
                            },
                        )
                        .await
                    {
                        Ok(control_id) => control_id,
                        Err(error) => {
                            conn.write_response(&Response::Error(ErrorResponse {
                                error: error.into_rmux_error(),
                            }))
                            .await?;
                            drop(detached_request_guard.take());
                            continue;
                        }
                    };
                    Some((
                        initial_command_count,
                        control_mode,
                        server_event_rx,
                        closing,
                        control_id,
                    ))
                } else {
                    None
                };

                let response_result = match (legacy_kill_server_wire, &outcome.response) {
                    (Some(wire_version), Response::KillServer(_)) => {
                        conn.write_legacy_kill_server_response(wire_version).await
                    }
                    _ => conn.write_response(&outcome.response).await,
                };
                if let Err(error) = response_result {
                    if let Some((_, _, _, _, control_id)) = pending_control.as_ref() {
                        handler.finish_control(requester.pid, *control_id).await;
                    }
                    drop(detached_request_guard.take());
                    #[cfg(windows)]
                    let _ = handler
                        .request_shutdown_if_pending_excluding_detached_connection(Some(connection_id));
                    return Err(error);
                }

                #[cfg(feature = "web")]
                if let Some(guard) = undelivered_web_share.as_mut() {
                    guard.disarm();
                }

                if let Some(attach) = outcome.attach {
                    let attach_forwarder_guard = attach_forwarder_guard
                        .expect("attach outcome must hold an attach forwarder guard");
                    let Response::AttachSession(response) = &outcome.response else {
                        return Err(io::Error::other(
                            "attach upgrade requires an attach-session response",
                        ));
                    };
                    let session_name = response.session_name.clone();
                    let terminal_context = attach.target.outer_terminal.context().clone();
                    let attach_identity = handler
                        .register_attach_identity_with_access(
                            requester.pid,
                            session_name.clone(),
                            Some(attach.session_id),
                            AttachRegistration {
                                control_tx: attach.control_tx,
                                control_backlog: attach.control_backlog.clone(),
                                closing: attach.closing.clone(),
                                persistent_overlay_epoch: attach.persistent_overlay_epoch.clone(),
                                terminal_context,
                                flags: attach.flags,
                                render_stream: attach.render_stream,
                                uid: requester.uid,
                                user: requester.user.clone(),
                                can_write,
                                client_size: attach.client_size,
                            },
                        )
                        .await
                        .ok_or_else(|| {
                            io::Error::other("attach session changed before registration")
                        })?;
                    drop(detached_connection_guard);
                    drop(detached_request_guard.take());
                    handler
                        .emit_client_attached_identity(
                            requester.pid,
                            session_name,
                            attach.session_id,
                        )
                        .await;
                    let (stream, buffered_bytes) = conn.into_raw_parts();
                    if !buffered_bytes.is_empty() {
                        warn!(
                            buffered = buffered_bytes.len(),
                            "preserving buffered bytes at attach upgrade boundary"
                        );
                    }
                    let result = pane_io::forward_attach(
                        stream,
                        attach.target,
                        buffered_bytes,
                        shutdown,
                        attach.control_rx,
                        attach.control_backlog,
                        attach.closing,
                        attach.persistent_overlay_epoch,
                        pane_io::LiveAttachInputContext::new(
                            Arc::clone(&handler),
                            attach_identity,
                        ),
                        attach.render_stream,
                    )
                    .await;
                    handler
                        .finish_attach(requester.pid, attach_identity.attach_id())
                        .await;
                    drop(attach_forwarder_guard);
                    let _ = handler.request_shutdown_if_pending();
                    return result;
                }
                if let Some((
                    initial_command_count,
                    control_mode,
                    server_event_rx,
                    closing,
                    control_id,
                )) = pending_control
                {
                    drop(detached_connection_guard);
                    drop(detached_request_guard.take());
                    let (stream, buffered_bytes) = conn.into_raw_parts();
                    let result = control::forward_control(
                        stream,
                        Arc::clone(&handler),
                        ControlClientIdentity::new(requester.pid, control_id),
                        ControlUpgradeInput::with_mode(
                            buffered_bytes,
                            initial_command_count,
                            control_mode,
                        ),
                        shutdown,
                        server_event_rx,
                        ControlLifecycle {
                            closing,
                            shutdown_handle: shutdown_handle.clone(),
                        },
                    )
                    .await;
                    handler.finish_control(requester.pid, control_id).await;
                    return result;
                }

                drop(detached_request_guard.take());
                if handler
                    .request_shutdown_if_pending_excluding_detached_connection(Some(connection_id))
                {
                    return Ok(());
                }
            }
            result = shutdown.changed() => {
                if result.is_ok() {
                    debug!("closing client connection during shutdown");
                }
                return Ok(());
            }
        }
    }
}

async fn write_prepared_sdk_wait(
    conn: &mut Connection,
    prepared: PreparedSdkWait,
    shutdown: &mut watch::Receiver<()>,
    handler: &Arc<RequestHandler>,
    connection_id: u64,
    detached_request_guard: &mut Option<DetachedRequestGuard>,
    send_armed_ack: bool,
) -> io::Result<bool> {
    let response = match prepared {
        PreparedSdkWait::Immediate(response) => response,
        PreparedSdkWait::Armed(wait) => {
            if send_armed_ack {
                conn.write_response(&wait.armed_response()).await?;
            }
            tokio::select! {
                response = wait.wait() => response,
                result = shutdown.changed() => {
                    if result.is_ok() {
                        debug!("closing client connection during shutdown");
                    }
                    return Ok(true);
                }
                result = wait_for_peer_close(&conn.stream) => {
                    result?;
                    debug!("closing client connection after peer disconnect");
                    return Ok(true);
                }
            }
        }
    };

    if let Err(error) = conn.write_response(&response).await {
        drop(detached_request_guard.take());
        #[cfg(windows)]
        let _ =
            handler.request_shutdown_if_pending_excluding_detached_connection(Some(connection_id));
        return Err(error);
    }

    drop(detached_request_guard.take());
    Ok(handler.request_shutdown_if_pending_excluding_detached_connection(Some(connection_id)))
}

async fn run_connection_with_cleanup(
    stream: LocalStream,
    requester: PeerIdentity,
    handler: Arc<RequestHandler>,
    connection_id: u64,
    shutdown: watch::Receiver<()>,
    shutdown_handle: ShutdownHandle,
) -> io::Result<()> {
    let mut cleanup_guard = ConnectionCleanupGuard::new(Arc::clone(&handler), connection_id);
    let result = serve_connection(
        stream,
        requester,
        handler,
        connection_id,
        shutdown,
        shutdown_handle,
    )
    .await;
    cleanup_guard.cleanup_now();
    result
}

struct ConnectionCleanupGuard {
    handler: Arc<RequestHandler>,
    connection_id: u64,
    active: bool,
}

impl ConnectionCleanupGuard {
    fn new(handler: Arc<RequestHandler>, connection_id: u64) -> Self {
        Self {
            handler,
            connection_id,
            active: true,
        }
    }

    fn cleanup_now(&mut self) {
        if !self.active {
            return;
        }
        self.handler
            .cleanup_connection_subscriptions_sync(self.connection_id);
        self.handler
            .cleanup_connection_pane_state_subscriptions_sync(self.connection_id);
        self.handler
            .cleanup_connection_sdk_waits_sync(self.connection_id);
        self.active = false;
    }
}

impl Drop for ConnectionCleanupGuard {
    fn drop(&mut self) {
        self.cleanup_now();
    }
}

fn request_enables_sdk_wait_armed_ack(request: &Request) -> bool {
    matches!(
        request,
        Request::Handshake(handshake)
            if handshake
                .required_capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_SDK_WAITS_ARMED)
    )
}

fn request_enables_session_lease_by_id(request: &Request) -> bool {
    matches!(
        request,
        Request::Handshake(handshake)
            if handshake
                .required_capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2)
    )
}

fn request_cancels_on_peer_disconnect(request: &Request) -> bool {
    matches!(
        request,
        Request::WaitFor(wait)
            if matches!(wait.mode, WaitForMode::Wait | WaitForMode::Lock)
    ) || matches!(
        request,
        Request::SdkWaitForOutput(_)
            | Request::SdkWaitForOutputRef(_)
            | Request::PaneStateCursor(rmux_proto::PaneStateCursorRequest { wait: true, .. })
    ) || matches!(
        request,
        Request::WebShare(web_share)
            if matches!(web_share.as_ref(), rmux_proto::WebShareRequest::Create(_))
    )
}

fn request_counts_as_detached_activity(request: &Request) -> bool {
    !matches!(
        request,
        Request::Handshake(_) | Request::DaemonStatus(_) | Request::ShutdownIfIdle(_)
    )
}

fn drain_finished_connection_tasks(tasks: &mut JoinSet<io::Result<()>>) {
    while let Some(result) = tasks.try_join_next() {
        log_connection_task_result(result);
    }
}

async fn drain_connection_tasks_for_shutdown(tasks: &mut JoinSet<io::Result<()>>) {
    let graceful = async {
        while let Some(result) = tasks.join_next().await {
            log_connection_task_result(result);
        }
    };
    if tokio::time::timeout(CONNECTION_SHUTDOWN_GRACE, graceful)
        .await
        .is_ok()
    {
        return;
    }

    warn!(
        remaining = tasks.len(),
        "aborting client connections that did not drain during daemon shutdown"
    );
    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        log_connection_task_result(result);
    }
}

fn log_connection_task_result(result: Result<io::Result<()>, JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!("connection error: {error}"),
        Err(error) if error.is_cancelled() => {
            debug!("connection task cancelled during daemon shutdown")
        }
        Err(error) => warn!("connection task failed: {error}"),
    }
}

struct Connection {
    stream: LocalStream,
    decoder: FrameDecoder,
    read_buffer: [u8; 8192],
}

struct IncomingRequest {
    request: Request,
    legacy_kill_server_wire: Option<PublishedLegacyWireVersion>,
}

impl IncomingRequest {
    fn current(request: Request) -> Self {
        Self {
            request,
            legacy_kill_server_wire: None,
        }
    }

    fn legacy_kill_server(wire_version: PublishedLegacyWireVersion) -> Self {
        Self {
            request: Request::KillServer(rmux_proto::KillServerRequest),
            legacy_kill_server_wire: Some(wire_version),
        }
    }
}

impl Connection {
    fn new(stream: LocalStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
            read_buffer: [0; 8192],
        }
    }

    async fn read_request(&mut self) -> io::Result<Option<IncomingRequest>> {
        loop {
            match inspect_legacy_kill_server_frame(self.decoder.remaining_bytes()) {
                LegacyKillServerFrame::Complete(wire_version) => {
                    self.decoder = FrameDecoder::new();
                    return Ok(Some(IncomingRequest::legacy_kill_server(wire_version)));
                }
                LegacyKillServerFrame::Incomplete => {}
                LegacyKillServerFrame::NotLegacyKillServer => {
                    match self.decoder.next_frame::<Request>() {
                        Ok(Some(request)) => return Ok(Some(IncomingRequest::current(request))),
                        Ok(None) => {}
                        Err(error) => {
                            let response = Response::Error(ErrorResponse { error });
                            self.write_response(&response).await?;
                            return Ok(None);
                        }
                    }
                }
            }

            let bytes_read = self.stream.read(&mut self.read_buffer).await?;
            if bytes_read == 0 {
                return Ok(None);
            }

            self.decoder.push_bytes(&self.read_buffer[..bytes_read]);
        }
    }

    async fn write_response(&mut self, response: &Response) -> io::Result<()> {
        let frame = encode_response_frame(response).map_err(io::Error::other)?;
        self.write_frame(&frame).await
    }

    async fn write_legacy_kill_server_response(
        &mut self,
        wire_version: PublishedLegacyWireVersion,
    ) -> io::Result<()> {
        let frame = encode_legacy_kill_server_response(wire_version);
        self.write_frame(&frame).await
    }

    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        tokio::time::timeout(
            DETACHED_RESPONSE_WRITE_TIMEOUT,
            self.stream.write_all(frame),
        )
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "detached client did not drain the response",
            )
        })?
    }

    fn into_raw_parts(self) -> (LocalStream, Vec<u8>) {
        let buffered_bytes = self.decoder.remaining_bytes().to_vec();
        (self.stream, buffered_bytes)
    }
}

fn encode_response_frame(response: &Response) -> Result<Vec<u8>, rmux_proto::RmuxError> {
    match encode_frame(response) {
        Ok(frame) => Ok(frame),
        Err(error @ rmux_proto::RmuxError::FrameTooLarge { .. }) => {
            // A handler must not turn one oversized, otherwise valid RPC
            // result into an unexplained transport disconnect. Keep the
            // stream synchronized and surface the typed codec failure.
            encode_frame(&Response::Error(ErrorResponse { error }))
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::server_access::AccessMode;
    use rmux_proto::{
        decode_frame, AttachSessionRequest, CancelSdkWaitResponse, ClientTerminalContext,
        ControlMode, ControlModeRequest, CreateSessionLeaseRequest, DaemonStatusRequest,
        ErrorResponse, HandshakeRequest, HasSessionRequest, ListSessionsRequest, NewSessionRequest,
        PaneOutputSubscriptionStart, PaneTarget, RenameSessionRequest, RmuxError,
        SdkWaitForOutputRequest, SdkWaitForOutputResponse, SdkWaitId, SdkWaitOutcome,
        SdkWaitOwnerId, SessionName, ShutdownIfIdleRequest, ShutdownIfIdleResponse, TerminalSize,
        WaitForMode, WaitForRequest, WaitForResponse, RMUX_WIRE_VERSION,
    };

    #[test]
    fn oversized_response_is_replaced_by_a_framed_error() {
        let response = Response::Error(ErrorResponse {
            error: RmuxError::Server("x".repeat(rmux_proto::DEFAULT_MAX_DETACHED_FRAME_LENGTH)),
        });
        let frame = encode_response_frame(&response).expect("fallback error encodes");
        assert!(matches!(
            decode_frame::<Response>(&frame).expect("fallback frame decodes"),
            Response::Error(ErrorResponse {
                error: RmuxError::FrameTooLarge { .. }
            })
        ));
    }

    #[tokio::test]
    async fn daemon_shutdown_aborts_a_non_reading_detached_response() -> io::Result<()> {
        let (server, client) = LocalStream::pair()?;
        let mut connection = Connection::new(server);
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            connection
                .write_response(&Response::Error(ErrorResponse {
                    error: RmuxError::Server("x".repeat(7_000_000)),
                }))
                .await
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(
            tasks.len(),
            1,
            "the unread response should still be blocked"
        );
        drain_connection_tasks_for_shutdown(&mut tasks).await;
        assert!(tasks.is_empty());
        drop(client);
        Ok(())
    }

    #[tokio::test]
    async fn excessive_initial_control_commands_are_rejected_before_upgrade_ack() -> io::Result<()>
    {
        let handler = Arc::new(RequestHandler::new());
        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;

        write_test_request(
            &mut client,
            Request::ControlMode(ControlModeRequest {
                mode: ControlMode::Plain,
                client_terminal: ClientTerminalContext::default(),
                initial_command_count: (rmux_proto::MAX_INITIAL_CONTROL_COMMANDS + 1) as u32,
            }),
        )
        .await?;

        let response = read_test_response(&mut client).await?;
        assert!(
            matches!(
                &response,
                Response::Error(ErrorResponse {
                    error: RmuxError::Server(message),
                }) if message.contains("too many initial control-mode commands")
            ),
            "the detached response must reject the command count before upgrading: {response:?}"
        );

        drop(client);
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn client_disconnect_cancels_plain_waiter() -> io::Result<()> {
        let handler = Arc::new(RequestHandler::new());
        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;

        write_test_request(&mut client, wait_for("disconnect-plain", WaitForMode::Wait)).await?;
        yield_until_counts(&handler, "disconnect-plain", (1, 0, false)).await;

        drop(client);

        yield_until_counts(&handler, "disconnect-plain", (0, 0, false)).await;
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn client_disconnect_cancels_queued_lock_waiter() -> io::Result<()> {
        let handler = Arc::new(RequestHandler::new());
        assert_eq!(
            handler
                .handle(wait_for("disconnect-lock", WaitForMode::Lock))
                .await,
            Response::WaitFor(WaitForResponse)
        );
        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;

        write_test_request(&mut client, wait_for("disconnect-lock", WaitForMode::Lock)).await?;
        yield_until_counts(&handler, "disconnect-lock", (0, 1, true)).await;

        drop(client);

        yield_until_counts(&handler, "disconnect-lock", (0, 0, true)).await;
        connection_task.await.expect("connection task")?;
        assert_eq!(
            handler
                .handle(wait_for("disconnect-lock", WaitForMode::Unlock))
                .await,
            Response::WaitFor(WaitForResponse)
        );
        assert!(matches!(
            handler
                .handle(wait_for("disconnect-lock", WaitForMode::Unlock))
                .await,
            Response::Error(ErrorResponse {
                error: RmuxError::Message(_),
            })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_if_idle_counts_other_open_detached_connections() -> io::Result<()> {
        let handler = Arc::new(RequestHandler::new());
        let (mut idle_client, _idle_shutdown_tx, idle_task) = spawn_test_connection(&handler)?;
        let (mut upgrade_client, _upgrade_shutdown_tx, upgrade_task) =
            spawn_test_connection(&handler)?;

        write_test_request(
            &mut idle_client,
            Request::Handshake(HandshakeRequest::current()),
        )
        .await?;
        assert!(matches!(
            read_test_response(&mut idle_client).await?,
            Response::Handshake(_)
        ));

        write_test_request(
            &mut upgrade_client,
            Request::DaemonStatus(DaemonStatusRequest),
        )
        .await?;
        let Response::DaemonStatus(status) = read_test_response(&mut upgrade_client).await? else {
            panic!("expected daemon status response");
        };
        assert_eq!(status.session_count, 0);
        assert_eq!(
            status.client_count, 1,
            "daemon-status must exclude its own detached connection but count another idle SDK connection"
        );

        write_test_request(
            &mut upgrade_client,
            Request::ShutdownIfIdle(ShutdownIfIdleRequest),
        )
        .await?;
        assert_eq!(
            read_test_response(&mut upgrade_client).await?,
            Response::ShutdownIfIdle(ShutdownIfIdleResponse {
                shutdown: false,
                session_count: 0,
                client_count: 1,
            })
        );

        drop(idle_client);
        idle_task.await.expect("idle connection task")?;

        write_test_request(
            &mut upgrade_client,
            Request::ShutdownIfIdle(ShutdownIfIdleRequest),
        )
        .await?;
        assert_eq!(
            read_test_response(&mut upgrade_client).await?,
            Response::ShutdownIfIdle(ShutdownIfIdleResponse {
                shutdown: true,
                session_count: 0,
                client_count: 0,
            })
        );
        upgrade_task.await.expect("upgrade connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn persistent_connection_reevaluates_server_access_per_request() -> io::Result<()> {
        let peer_uid = rmux_os::identity::real_user_id().saturating_add(10_000);
        let peer = PeerIdentity {
            pid: std::process::id(),
            uid: peer_uid,
            user: rmux_os::identity::UserIdentity::Uid(peer_uid),
        };
        let handler = Arc::new(RequestHandler::new());
        handler
            .set_test_access_mode_for_uid(peer_uid, AccessMode::ReadWrite)
            .expect("test peer starts read-write");
        let (mut client, _shutdown_tx, connection_task) =
            spawn_test_connection_with_peer(&handler, peer)?;

        write_test_request(&mut client, rename_missing_session_request()).await?;
        let response = read_test_response(&mut client).await?;
        match response {
            Response::Error(ErrorResponse {
                error: RmuxError::Server(message),
            }) => {
                assert_ne!(message, "client is read-only");
                assert_ne!(message, "access not allowed");
            }
            Response::Error(_) => {}
            response => panic!("expected rename-session to reach the handler, got {response:?}"),
        }

        handler
            .set_test_access_mode_for_uid(peer_uid, AccessMode::ReadOnly)
            .expect("test peer downgrades to read-only");
        write_test_request(&mut client, rename_missing_session_request()).await?;
        assert_eq!(
            read_test_response(&mut client).await?,
            Response::Error(ErrorResponse {
                error: RmuxError::Server("client is read-only".to_owned())
            })
        );

        handler
            .remove_test_access_for_uid(peer_uid)
            .expect("test peer access can be revoked");
        write_test_request(
            &mut client,
            Request::ListSessions(ListSessionsRequest {
                format: None,
                filter: None,
                sort_order: None,
                reversed: false,
            }),
        )
        .await?;
        assert_eq!(
            read_test_response(&mut client).await?,
            Response::Error(ErrorResponse {
                error: RmuxError::Server("access not allowed".to_owned())
            })
        );

        drop(client);
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn sdk_wait_connection_writes_armed_ack_before_final_match() -> io::Result<()> {
        let handler = Arc::new(RequestHandler::new());
        let session = SessionName::new("sdkack").expect("valid session");
        let target = PaneTarget::new(session.clone(), 0);
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
        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;

        write_test_request(
            &mut client,
            Request::Handshake(HandshakeRequest::requiring([CAPABILITY_SDK_WAITS_ARMED])),
        )
        .await?;
        assert!(matches!(
            read_test_response(&mut client).await?,
            Response::Handshake(_)
        ));

        write_test_request(
            &mut client,
            Request::SdkWaitForOutput(SdkWaitForOutputRequest {
                target: target.clone(),
                bytes: b"needle".to_vec(),
                start: PaneOutputSubscriptionStart::Now,
                owner_id: SdkWaitOwnerId::new(7),
                wait_id: SdkWaitId::new(11),
            }),
        )
        .await?;

        assert_eq!(
            read_test_response(&mut client).await?,
            Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(SdkWaitId::new(11)))
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), read_test_response(&mut client))
                .await
                .is_err(),
            "SDK wait must remain pending after the daemon-side armed ack"
        );

        handler
            .send_pane_output_for_test(&target, b"needle".to_vec())
            .await;

        assert_eq!(
            read_test_response(&mut client).await?,
            Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                wait_id: SdkWaitId::new(11),
                outcome: SdkWaitOutcome::Matched,
            })
        );
        drop(client);
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn negotiated_session_lease_by_id_survives_name_reuse_before_creation() -> io::Result<()>
    {
        let handler = Arc::new(RequestHandler::new());
        let original_name = SessionName::new("lease-negotiated-owner").expect("valid session");
        let renamed = SessionName::new("lease-negotiated-renamed").expect("valid session");
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: original_name.clone(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
        assert!(matches!(
            handler
                .handle(Request::RenameSession(RenameSessionRequest {
                    target: original_name.clone(),
                    new_name: renamed.clone(),
                }))
                .await,
            Response::RenameSession(_)
        ));
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: original_name.clone(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));

        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;
        write_test_request(
            &mut client,
            Request::Handshake(HandshakeRequest::requiring([
                CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
            ])),
        )
        .await?;
        assert!(matches!(
            read_test_response(&mut client).await?,
            Response::Handshake(_)
        ));

        write_test_request(
            &mut client,
            Request::CreateSessionLease(CreateSessionLeaseRequest {
                session_name: SessionName::new("$0").expect("stable session target"),
                ttl_millis: 600,
            }),
        )
        .await?;
        assert!(matches!(
            read_test_response(&mut client).await?,
            Response::CreateSessionLease(_)
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let renamed_exists = handler
                .handle(Request::HasSession(HasSessionRequest {
                    target: renamed.clone(),
                }))
                .await;
            if matches!(
                renamed_exists,
                Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
            ) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "stable-id lease did not reap the originally created session"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            handler
                .handle(Request::HasSession(HasSessionRequest {
                    target: original_name,
                }))
                .await,
            Response::HasSession(rmux_proto::HasSessionResponse { exists: true }),
            "the session that reused the nominal name must survive"
        );

        drop(client);
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_sdk_wait_connection_does_not_receive_armed_ack() -> io::Result<()> {
        let handler = Arc::new(RequestHandler::new());
        let session = SessionName::new("sdklegacy").expect("valid session");
        let target = PaneTarget::new(session.clone(), 0);
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
        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;

        write_test_request(
            &mut client,
            Request::SdkWaitForOutput(SdkWaitForOutputRequest {
                target: target.clone(),
                bytes: b"needle".to_vec(),
                start: PaneOutputSubscriptionStart::Now,
                owner_id: SdkWaitOwnerId::new(7),
                wait_id: SdkWaitId::new(11),
            }),
        )
        .await?;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), read_test_response(&mut client))
                .await
                .is_err(),
            "legacy SDK wait clients must not receive the two-phase armed ack"
        );

        handler
            .send_pane_output_for_test(&target, b"needle".to_vec())
            .await;

        assert_eq!(
            read_test_response(&mut client).await?,
            Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                wait_id: SdkWaitId::new(11),
                outcome: SdkWaitOutcome::Matched,
            })
        );
        drop(client);
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn published_legacy_kill_server_frames_dispatch_and_ack_shutdown() -> io::Result<()> {
        for wire_version in 1..=3 {
            let handler = Arc::new(RequestHandler::new());
            let (server, mut client) = LocalStream::pair()?;
            let (connection_shutdown_tx, connection_shutdown_rx) = watch::channel(());
            let (shutdown_handle, shutdown_request_rx) = ShutdownHandle::new();
            handler.install_shutdown_handle(shutdown_handle.clone());
            let requester = PeerIdentity {
                pid: std::process::id(),
                uid: rmux_os::identity::real_user_id(),
                user: rmux_os::identity::UserIdentity::Uid(rmux_os::identity::real_user_id()),
            };
            let connection_id = handler.allocate_connection_id();
            let connection_handler = Arc::clone(&handler);
            let connection_task = tokio::spawn(async move {
                run_connection_with_cleanup(
                    server,
                    requester,
                    connection_handler,
                    connection_id,
                    connection_shutdown_rx,
                    shutdown_handle,
                )
                .await
            });

            let frame = raw_legacy_kill_server_frame(wire_version);
            for byte in frame {
                client.write_all(&[byte]).await?;
                tokio::task::yield_now().await;
            }

            let mut response = [0_u8; 10];
            client.read_exact(&mut response).await?;
            assert_eq!(response[0], rmux_proto::RMUX_FRAME_MAGIC);
            assert_eq!(response[1], wire_version);
            assert_eq!(&response[2..6], &4_u32.to_le_bytes());
            assert_eq!(&response[6..], &63_u32.to_le_bytes());

            tokio::time::timeout(Duration::from_secs(2), shutdown_request_rx)
                .await
                .expect("legacy kill-server should request daemon shutdown")
                .expect("shutdown receiver should complete cleanly");
            drop(client);
            let _ = connection_shutdown_tx.send(());
            connection_task.await.expect("connection task")?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn legacy_envelope_exception_rejects_non_kill_and_unpublished_frames() -> io::Result<()> {
        let mut other_request = raw_legacy_kill_server_frame(3);
        other_request[6..10].copy_from_slice(&71_u32.to_le_bytes());

        let mut trailing_payload = raw_legacy_kill_server_frame(3);
        trailing_payload[2..6].copy_from_slice(&5_u32.to_le_bytes());
        trailing_payload.push(0);

        let unpublished_wire = raw_legacy_kill_server_frame(4);

        for frame in [other_request, trailing_payload, unpublished_wire] {
            let (server, mut client) = LocalStream::pair()?;
            let mut connection = Connection::new(server);
            let read_task = tokio::spawn(async move { connection.read_request().await });
            client.write_all(&frame).await?;

            let response = read_test_response(&mut client).await?;
            assert!(matches!(
                response,
                Response::Error(ErrorResponse {
                    error: RmuxError::UnsupportedWireVersion { .. },
                })
            ));
            assert!(read_task.await.expect("read task")?.is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn read_request_sends_framed_error_for_unsupported_wire_version() -> io::Result<()> {
        let (server, mut client) = LocalStream::pair()?;
        let mut connection = Connection::new(server);
        let read_task = tokio::spawn(async move { connection.read_request().await });

        let mut frame = encode_frame(&wait_for("bad-wire-version", WaitForMode::Signal))
            .map_err(io::Error::other)?;
        assert_eq!(frame.get(1).copied(), Some(RMUX_WIRE_VERSION as u8));
        frame[1] = RMUX_WIRE_VERSION.saturating_add(1) as u8;
        client.write_all(&frame).await?;

        let response = read_test_response(&mut client).await?;
        assert!(matches!(
            response,
            Response::Error(ErrorResponse {
                error: RmuxError::UnsupportedWireVersion { .. },
            })
        ));
        assert!(read_task.await.expect("read task")?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn read_request_sends_framed_error_for_decode_mismatch() -> io::Result<()> {
        let (server, mut client) = LocalStream::pair()?;
        let mut connection = Connection::new(server);
        let read_task = tokio::spawn(async move { connection.read_request().await });

        let payload = 255_u32.to_le_bytes();
        let mut frame = vec![rmux_proto::RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION as u8];
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        client.write_all(&frame).await?;

        let response = read_test_response(&mut client).await?;
        assert!(matches!(
            response,
            Response::Error(ErrorResponse {
                error: RmuxError::Decode(_),
            })
        ));
        assert!(read_task.await.expect("read task")?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn attach_upgrade_preserves_only_already_read_bounded_remainder() -> io::Result<()> {
        let (server, mut client) = LocalStream::pair()?;
        let mut connection = Connection::new(server);
        let read_capacity = connection.read_buffer.len();
        let mut bytes = encode_frame(&Request::AttachSession(AttachSessionRequest {
            target: SessionName::new("alpha").expect("valid session"),
        }))
        .map_err(io::Error::other)?;
        let extra = vec![b'x'; read_capacity + 256];
        bytes.extend_from_slice(&extra);

        let writer = tokio::spawn(async move {
            client.write_all(&bytes).await?;
            client.shutdown().await
        });

        let request = tokio::time::timeout(Duration::from_secs(5), connection.read_request())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "read_request timed out"))??;
        assert!(matches!(
            request,
            Some(IncomingRequest {
                request: Request::AttachSession(_),
                legacy_kill_server_wire: None,
            })
        ));
        let (mut stream, buffered_bytes) = connection.into_raw_parts();
        let mut stream_remainder = Vec::new();
        stream.read_to_end(&mut stream_remainder).await?;
        writer.await.expect("writer task")?;

        assert!(
            buffered_bytes.len() <= read_capacity,
            "attach upgrade remainder must be bounded by the server read buffer"
        );
        assert!(
            buffered_bytes.len() < extra.len(),
            "bytes not yet read by the detached decoder must remain in the stream"
        );
        assert_eq!(&buffered_bytes, &extra[..buffered_bytes.len()]);

        let mut observed_extra = buffered_bytes;
        observed_extra.extend_from_slice(&stream_remainder);
        assert_eq!(observed_extra, extra);
        Ok(())
    }

    #[tokio::test]
    async fn handshake_rejects_unsupported_wire_version_range() -> io::Result<()> {
        let handler = Arc::new(RequestHandler::new());
        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;

        write_test_request(
            &mut client,
            Request::Handshake(HandshakeRequest {
                minimum_wire_version: RMUX_WIRE_VERSION + 1,
                maximum_wire_version: RMUX_WIRE_VERSION + 1,
                required_capabilities: Vec::new(),
            }),
        )
        .await?;

        let response = read_test_response(&mut client).await?;
        assert!(matches!(
            response,
            Response::Error(ErrorResponse {
                error: RmuxError::UnsupportedWireVersion { .. },
            })
        ));

        drop(client);
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    #[tokio::test]
    async fn handshake_rejects_unsupported_required_capability() -> io::Result<()> {
        let handler = Arc::new(RequestHandler::new());
        let (mut client, _shutdown_tx, connection_task) = spawn_test_connection(&handler)?;

        write_test_request(
            &mut client,
            Request::Handshake(HandshakeRequest::requiring(["capability.future"])),
        )
        .await?;

        let response = read_test_response(&mut client).await?;
        match response {
            Response::Error(ErrorResponse {
                error: RmuxError::UnsupportedCapability { feature, supported },
            }) => {
                assert_eq!(feature, "capability.future");
                assert!(supported
                    .iter()
                    .any(|capability| capability == "rpc.detached"));
            }
            response => panic!("expected unsupported capability error, got {response:?}"),
        }

        drop(client);
        connection_task.await.expect("connection task")?;
        Ok(())
    }

    fn spawn_test_connection(
        handler: &Arc<RequestHandler>,
    ) -> io::Result<(
        LocalStream,
        watch::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    )> {
        spawn_test_connection_with_peer(
            handler,
            PeerIdentity {
                pid: std::process::id(),
                uid: rmux_os::identity::real_user_id(),
                user: rmux_os::identity::UserIdentity::Uid(rmux_os::identity::real_user_id()),
            },
        )
    }

    fn spawn_test_connection_with_peer(
        handler: &Arc<RequestHandler>,
        peer: PeerIdentity,
    ) -> io::Result<(
        LocalStream,
        watch::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    )> {
        let (server, client) = LocalStream::pair()?;
        let handler = Arc::clone(handler);
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
        let connection_id = handler.allocate_connection_id();
        let task = tokio::spawn(async move {
            run_connection_with_cleanup(
                server,
                peer,
                handler,
                connection_id,
                shutdown_rx,
                shutdown_handle,
            )
            .await
        });
        Ok((client, shutdown_tx, task))
    }

    fn rename_missing_session_request() -> Request {
        Request::RenameSession(RenameSessionRequest {
            target: SessionName::new("missing").expect("valid source session"),
            new_name: SessionName::new("renamed").expect("valid destination session"),
        })
    }

    fn wait_for(channel: &str, mode: WaitForMode) -> Request {
        Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode,
        })
    }

    fn raw_legacy_kill_server_frame(wire_version: u8) -> Vec<u8> {
        let mut frame = vec![rmux_proto::RMUX_FRAME_MAGIC, wire_version];
        frame.extend_from_slice(&4_u32.to_le_bytes());
        frame.extend_from_slice(&72_u32.to_le_bytes());
        frame
    }

    async fn write_test_request(stream: &mut LocalStream, request: Request) -> io::Result<()> {
        let frame = encode_frame(&request).map_err(io::Error::other)?;
        stream.write_all(&frame).await
    }

    async fn read_test_response(stream: &mut LocalStream) -> io::Result<Response> {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 512];

        loop {
            if let Some(response) = decoder.next_frame::<Response>().map_err(io::Error::other)? {
                return Ok(response);
            }

            let bytes_read = stream.read(&mut buffer).await?;
            if bytes_read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed before response frame",
                ));
            }
            decoder.push_bytes(&buffer[..bytes_read]);
        }
    }

    async fn yield_until_counts(
        handler: &RequestHandler,
        channel: &str,
        expected: (usize, usize, bool),
    ) {
        for _ in 0..200 {
            if handler.wait_for_counts(channel) == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        assert_eq!(handler.wait_for_counts(channel), expected);
    }
}

#[cfg(all(test, unix, feature = "web"))]
#[path = "listener_web_share_tests.rs"]
mod web_share_tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::io::Write as _;

    use rmux_proto::KillServerRequest;

    #[tokio::test]
    async fn kill_server_peer_disconnect_still_requests_shutdown() -> io::Result<()> {
        let endpoint = rmux_ipc::endpoint_for_label(format!(
            "listener-kill-disconnect-{}",
            std::process::id()
        ))?;
        let listener = rmux_ipc::LocalListener::bind(&endpoint)?;
        let handler = Arc::new(RequestHandler::new());
        let (connection_shutdown_tx, connection_shutdown_rx) = watch::channel(());
        let (shutdown_handle, shutdown_request_rx) = ShutdownHandle::new();
        handler.install_shutdown_handle(shutdown_handle.clone());

        let connection_handler = Arc::clone(&handler);
        let connection_task = tokio::spawn(async move {
            let (server, requester) = listener.accept().await?;
            let connection_id = connection_handler.allocate_connection_id();
            run_connection_with_cleanup(
                server,
                requester,
                connection_handler,
                connection_id,
                connection_shutdown_rx,
                shutdown_handle,
            )
            .await
        });

        let frame =
            encode_frame(&Request::KillServer(KillServerRequest)).map_err(io::Error::other)?;
        let endpoint_for_client = endpoint.clone();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            let mut client =
                rmux_ipc::connect_blocking(&endpoint_for_client, Duration::from_secs(2))?;
            client.write_all(&frame)?;
            Ok(())
        })
        .await
        .expect("client task should not panic")?;

        tokio::time::timeout(Duration::from_secs(2), shutdown_request_rx)
            .await
            .expect("kill-server should request daemon shutdown")
            .expect("shutdown receiver should complete cleanly");
        let _ = connection_shutdown_tx.send(());

        match connection_task.await.expect("connection task") {
            Ok(()) => Ok(()),
            Err(error) if rmux_ipc::is_peer_disconnect(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }
}
