use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmux_ipc::{LocalListener, LocalStream, PeerIdentity};
use rmux_proto::{encode_frame, ErrorResponse, FrameDecoder, Request, Response};
use rustix::net::RecvFlags;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinError, JoinSet};
use tracing::{debug, warn};

use crate::control::{self, ControlLifecycle, ControlServerEvent};
use crate::daemon::ShutdownHandle;
use crate::handler::{attach_support::AttachRegistration, RequestHandler};
use crate::pane_io;
use crate::server_access::apply_access_policy;
use crate::ConfigLoadOptions;

/// Accept loop: spawns a per-connection task for each incoming client.
pub(crate) async fn serve(
    listener: LocalListener,
    socket_path: PathBuf,
    shutdown_handle: ShutdownHandle,
    mut shutdown: oneshot::Receiver<()>,
    config_load: ConfigLoadOptions,
    owner_uid: u32,
) -> io::Result<()> {
    let _cleanup_on_drop = SocketCleanup::new(socket_path.clone());
    let handler = Arc::new(RequestHandler::with_owner_uid(owner_uid));
    handler.install_shutdown_handle(shutdown_handle.clone());
    handler.set_socket_path(&socket_path);
    handler.load_startup_config(config_load).await;
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
        tokio::select! {
            result = listener.accept() => {
                let (stream, requester) = result?;
                let handler = Arc::clone(&handler);
                let connection_shutdown = connection_shutdown_rx.clone();
                let shutdown_handle = shutdown_handle.clone();

                connection_tasks.spawn(async move {
                    serve_connection(stream, requester, handler, connection_shutdown, shutdown_handle).await
                });
            }
            Some(result) = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                log_connection_task_result(result);
            }
            _ = &mut shutdown => {
                debug!("shutdown requested");
                break;
            }
        }
    }

    drop(connection_shutdown);

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
    mut shutdown: watch::Receiver<()>,
    shutdown_handle: ShutdownHandle,
) -> io::Result<()> {
    let Some(access_mode) = handler.access_mode_for_peer(&requester) else {
        let mut conn = Connection::new(stream);
        conn.write_response(&Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("access not allowed".to_owned()),
        }))
        .await?;
        return Ok(());
    };
    let mut conn = Connection::new(stream);

    loop {
        tokio::select! {
            request = conn.read_request() => {
                let Some(request) = request? else {
                    return Ok(());
                };
                let request = match apply_access_policy(request, access_mode.can_write()) {
                    Ok(request) => request,
                    Err(error) => {
                        conn.write_response(&Response::Error(ErrorResponse { error })).await?;
                        continue;
                    }
                };

                debug!("dispatching {}", request.command_name());
                let outcome = tokio::select! {
                    outcome = handler.dispatch(requester.pid, request) => outcome,
                    result = shutdown.changed() => {
                        if result.is_ok() {
                            debug!("closing client connection during shutdown");
                        }
                        return Ok(());
                    }
                    result = wait_for_peer_close(&conn.stream) => {
                        result?;
                        debug!("closing client connection after peer disconnect");
                        return Ok(());
                    }
                };
                conn.write_response(&outcome.response).await?;
                if handler.request_shutdown_if_pending() {
                    return Ok(());
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
                                closing: attach.closing.clone(),
                                persistent_overlay_epoch: attach.persistent_overlay_epoch.clone(),
                                terminal_context,
                                flags: attach.flags,
                                uid: requester.uid,
                                can_write: access_mode.can_write(),
                                client_size: attach.client_size,
                            },
                        )
                        .await;
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
                        attach.closing,
                        attach.persistent_overlay_epoch,
                        pane_io::LiveAttachInputContext {
                            handler: Arc::clone(&handler),
                            attach_pid: requester.pid,
                        },
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
                            server_event_tx,
                            closing.clone(),
                            requester.uid,
                            access_mode.can_write(),
                        )
                        .await;
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

async fn wait_for_peer_close(stream: &LocalStream) -> io::Result<()> {
    loop {
        if let Err(error) = stream.readable().await {
            if is_peer_disconnect(&error) {
                return Ok(());
            }
            return Err(error);
        }
        let mut probe = [0_u8; 1];

        match rustix::net::recv(stream, &mut probe, RecvFlags::PEEK) {
            Ok((_initialized, 0)) => return Ok(()),
            Ok((_initialized, _available)) => return std::future::pending().await,
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
            Err(rustix::io::Errno::PIPE | rustix::io::Errno::CONNRESET) => return Ok(()),
            Err(error) => return Err(io::Error::from(error)),
        }
    }
}

fn is_peer_disconnect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    )
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

struct SocketCleanup {
    socket_path: PathBuf,
}

impl SocketCleanup {
    fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = remove_socket_file_if_present(&self.socket_path);
    }
}

fn remove_socket_file_if_present(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{ErrorResponse, RmuxError, WaitForMode, WaitForRequest, WaitForResponse};

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

    fn spawn_test_connection(
        handler: &Arc<RequestHandler>,
    ) -> io::Result<(
        LocalStream,
        watch::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    )> {
        let (server, client) = LocalStream::pair()?;
        let handler = Arc::clone(handler);
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
        let task = tokio::spawn(async move {
            serve_connection(
                server,
                PeerIdentity {
                    pid: std::process::id(),
                    uid: rmux_os::identity::real_user_id(),
                    user: rmux_os::identity::UserIdentity::Uid(rmux_os::identity::real_user_id()),
                },
                handler,
                shutdown_rx,
                shutdown_handle,
            )
            .await
        });
        Ok((client, shutdown_tx, task))
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
