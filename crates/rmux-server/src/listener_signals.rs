use std::path::Path;
use std::sync::Arc;

use rmux_ipc::LocalListener;
use tracing::debug;
#[cfg(unix)]
use tracing::warn;

use crate::daemon::ShutdownHandle;
use crate::handler::RequestHandler;
use crate::signals::{ServerSignal, SignalWatcher};
use crate::socket_cleanup::SocketCleanup;

pub(crate) fn poll_server_signal(server_signals: &Option<SignalWatcher>) -> Option<ServerSignal> {
    server_signals.as_ref().and_then(SignalWatcher::poll)
}

#[cfg(unix)]
pub(crate) async fn wait_server_signal(
    server_signals: &Option<SignalWatcher>,
) -> std::io::Result<()> {
    match server_signals {
        Some(watcher) => watcher.wait().await,
        None => std::future::pending().await,
    }
}

#[cfg(not(unix))]
pub(crate) async fn wait_server_signal(
    _server_signals: &Option<SignalWatcher>,
) -> std::io::Result<()> {
    std::future::pending().await
}

pub(crate) async fn handle_server_signal(
    signal: Option<ServerSignal>,
    shutdown_handle: &ShutdownHandle,
    handler: &Arc<RequestHandler>,
    socket_path: &Path,
    listener: &mut LocalListener,
    cleanup: &mut SocketCleanup,
) {
    match signal {
        Some(ServerSignal::Shutdown(reason)) => {
            debug!(reason, "requesting shutdown after server signal");
            shutdown_handle.request_shutdown();
        }
        Some(ServerSignal::ChildChanged) => {
            handler.continue_stopped_panes().await;
        }
        Some(ServerSignal::RecreateSocket) => {
            recreate_listener_after_signal(handler, socket_path, listener, cleanup).await;
        }
        None => {}
    }
}

#[cfg(unix)]
async fn recreate_listener_after_signal(
    handler: &RequestHandler,
    socket_path: &Path,
    listener: &mut LocalListener,
    cleanup: &mut SocketCleanup,
) {
    match handler
        .rebind_unix_socket(socket_path, cleanup.socket_identity())
        .await
    {
        Ok(rebound) => {
            *listener = rebound.listener;
            cleanup.update_socket_identity(rebound.identity);
            debug!(path = %socket_path.display(), "recreated Unix daemon socket after signal");
        }
        Err(error) => {
            warn!(path = %socket_path.display(), "failed to recreate Unix daemon socket after signal: {error}");
        }
    }
}

#[cfg(not(unix))]
async fn recreate_listener_after_signal(
    _handler: &RequestHandler,
    _socket_path: &Path,
    _listener: &mut LocalListener,
    _cleanup: &mut SocketCleanup,
) {
}
