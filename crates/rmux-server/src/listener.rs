use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmux_ipc::{wait_for_peer_close, LocalListener, LocalStream, PeerIdentity};
use rmux_proto::{encode_frame, ErrorResponse, FrameDecoder, Request, Response, WaitForMode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinError, JoinSet};
use tracing::{debug, warn};

use crate::control::{self, ControlLifecycle, ControlServerEvent};
use crate::daemon::ShutdownHandle;
use crate::handler::{attach_support::AttachRegistration, ControlRegistration, RequestHandler};
use crate::listener_options::ServeOptions;
use crate::listener_signals::handle_server_signal;
use crate::listener_signals::poll_server_signal;
use crate::listener_signals::wait_server_signal;
use crate::pane_io;
use crate::server_access::apply_access_policy;
use crate::socket_cleanup::SocketCleanup;

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
    #[cfg(all(any(unix, windows), feature = "web"))]
    if web_required {
        handler
            .ensure_web_share_listener_running()
            .await
            .map_err(|error| io::Error::new(io::ErrorKind::AddrNotAvailable, error.to_string()))?;
    }
    let startup_guard = handler.start_config_loading();
    let startup_handler = Arc::clone(&handler);
    let startup_config = options.config_load;
    let startup_task = tokio::spawn(async move {
        startup_handler
            .load_startup_config_with_guard(startup_config, startup_guard)
            .await;
    });
    let (connection_shutdown, connection_shutdown_rx) = watch::channel(());
    let mut connection_tasks = JoinSet::new();
    let hook_handler = Arc::clone(&handler);
    let hook_events = handler.subscribe_lifecycle_events();
    let hook_shutdown = connection_shutdown_rx.clone();
    let hook_task = tokio::spawn(async move {
        hook_handler
            .consume_lifecycle_hooks(hook_events, hook_shutdown)
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
                cleanup_on_drop.cleanup_now();
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

    while let Some(result) = connection_tasks.join_next().await {
        log_connection_task_result(result);
    }
    if let Err(error) = hook_task.await {
        warn!("lifecycle hook task failed: {error}");
    }

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

    loop {
        tokio::select! {
            request = conn.read_request() => {
                let Some(request) = request? else {
                    return Ok(());
                };
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
                let mut detached_request_guard = request_counts_as_detached_activity(&request)
                    .then(|| handler.begin_detached_request());

                let cancel_on_peer_disconnect = request_cancels_on_peer_disconnect(&request);
                debug!("dispatching {}", request.command_name());
                let outcome = tokio::select! {
                    outcome = handler.dispatch_for_connection(requester.pid, connection_id, request) => outcome,
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
                };
                if let Err(error) = conn.write_response(&outcome.response).await {
                    drop(detached_request_guard.take());
                    #[cfg(windows)]
                    let _ = handler
                        .request_shutdown_if_pending_excluding_detached_connection(Some(connection_id));
                    return Err(error);
                }

                if let Some(attach) = outcome.attach {
                    let Response::AttachSession(response) = &outcome.response else {
                        return Err(io::Error::other(
                            "attach upgrade requires an attach-session response",
                        ));
                    };
                    let session_name = response.session_name.clone();
                    let terminal_context = attach.target.outer_terminal.context().clone();
                    let attach_id = handler
                        .register_attach_with_access(
                            requester.pid,
                            session_name.clone(),
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
                        .await;
                    drop(detached_connection_guard);
                    drop(detached_request_guard.take());
                    handler.emit_client_attached(requester.pid, session_name).await;
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
                        pane_io::LiveAttachInputContext {
                            handler: Arc::clone(&handler),
                            attach_pid: requester.pid,
                        },
                        attach.render_stream,
                    )
                    .await;
                    handler.finish_attach(requester.pid, attach_id).await;
                    return result;
                }
                if let Some(control_upgrade) = outcome.control {
                    let (server_event_tx, server_event_rx) = tokio::sync::mpsc::unbounded_channel::<ControlServerEvent>();
                    let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let control_id = handler
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
                        .await;
                    drop(detached_connection_guard);
                    drop(detached_request_guard.take());
                    let (stream, buffered_bytes) = conn.into_raw_parts();
                    let result = control::forward_control(
                        stream,
                        Arc::clone(&handler),
                        requester.pid,
                        buffered_bytes,
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
            .cleanup_connection_sdk_waits_sync(self.connection_id);
        self.active = false;
    }
}

impl Drop for ConnectionCleanupGuard {
    fn drop(&mut self) {
        self.cleanup_now();
    }
}

fn request_cancels_on_peer_disconnect(request: &Request) -> bool {
    matches!(
        request,
        Request::WaitFor(wait)
            if matches!(wait.mode, WaitForMode::Wait | WaitForMode::Lock)
    ) || matches!(
        request,
        Request::SdkWaitForOutput(_) | Request::SdkWaitForOutputRef(_)
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

fn log_connection_task_result(result: Result<io::Result<()>, JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!("connection error: {error}"),
        Err(error) => warn!("connection task failed: {error}"),
    }
}

struct Connection {
    stream: LocalStream,
    decoder: FrameDecoder,
    read_buffer: [u8; 8192],
}

impl Connection {
    fn new(stream: LocalStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
            read_buffer: [0; 8192],
        }
    }

    async fn read_request(&mut self) -> io::Result<Option<Request>> {
        loop {
            match self.decoder.next_frame::<Request>() {
                Ok(Some(request)) => return Ok(Some(request)),
                Ok(None) => {}
                Err(error) => {
                    let response = Response::Error(ErrorResponse { error });
                    self.write_response(&response).await?;
                    return Ok(None);
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
        let frame = encode_frame(response).map_err(io::Error::other)?;
        self.stream.write_all(&frame).await
    }

    fn into_raw_parts(self) -> (LocalStream, Vec<u8>) {
        let buffered_bytes = self.decoder.remaining_bytes().to_vec();
        (self.stream, buffered_bytes)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::server_access::AccessMode;
    use rmux_proto::{
        DaemonStatusRequest, ErrorResponse, HandshakeRequest, ListSessionsRequest,
        RenameSessionRequest, RmuxError, SessionName, ShutdownIfIdleRequest,
        ShutdownIfIdleResponse, WaitForMode, WaitForRequest, WaitForResponse, RMUX_WIRE_VERSION,
    };

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
