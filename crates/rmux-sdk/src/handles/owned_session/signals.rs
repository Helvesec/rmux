//! Opt-in process signal listener for owned sessions.

#[cfg(windows)]
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(unix)]
use std::thread;

#[cfg(windows)]
use tokio::task::JoinHandle;

use crate::transport::TransportClient;
#[cfg(unix)]
use crate::RmuxError;
use crate::{Result, SessionName};
use rmux_proto::{KillSessionRequest, Request};

pub(super) fn install_default_signal_handlers(
    transport: TransportClient,
    target: SessionName,
    installed: Arc<AtomicBool>,
) -> Result<OwnedSessionSignalHandlers> {
    #[cfg(unix)]
    {
        install_unix_signal_handlers(transport, target, installed)
    }
    #[cfg(windows)]
    {
        Ok(install_tokio_signal_handlers(transport, target, installed))
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = (transport, target);
        Ok(OwnedSessionSignalHandlers { installed })
    }
}

#[cfg(windows)]
fn install_tokio_signal_handlers(
    transport: TransportClient,
    target: SessionName,
    installed: Arc<AtomicBool>,
) -> OwnedSessionSignalHandlers {
    let task = tokio::spawn(async move {
        wait_for_default_shutdown_signal().await;
        let _ = transport
            .request(Request::KillSession(KillSessionRequest {
                target,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
    });

    OwnedSessionSignalHandlers { task, installed }
}

#[cfg(unix)]
fn install_unix_signal_handlers(
    transport: TransportClient,
    target: SessionName,
    installed: Arc<AtomicBool>,
) -> Result<OwnedSessionSignalHandlers> {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
        RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "owned session signal handlers require a Tokio runtime: {error}"
        )))
    })?;
    let mut signals =
        Signals::new([SIGINT, SIGTERM, SIGHUP]).map_err(|source| RmuxError::Transport {
            operation: "install owned-session signal handlers".to_owned(),
            source,
        })?;
    let handle = signals.handle();
    let thread = thread::Builder::new()
        .name("rmux-sdk-owned-signals".to_owned())
        .spawn(move || {
            if signals.forever().next().is_some() {
                runtime.spawn(async move {
                    let _ = transport
                        .request(Request::KillSession(KillSessionRequest {
                            target,
                            kill_all_except_target: false,
                            clear_alerts: false,
                            kill_group: false,
                        }))
                        .await;
                });
            }
        })
        .map_err(|source| RmuxError::Transport {
            operation: "start owned-session signal handler".to_owned(),
            source,
        })?;

    Ok(OwnedSessionSignalHandlers {
        signal_handle: handle,
        thread: Some(thread),
        installed,
    })
}

/// Guard returned by [`OwnedSession::install_default_signal_handlers`](super::OwnedSession::install_default_signal_handlers).
#[derive(Debug)]
#[must_use = "dropping this guard disables the installed signal listener"]
pub struct OwnedSessionSignalHandlers {
    #[cfg(unix)]
    signal_handle: signal_hook::iterator::Handle,
    #[cfg(unix)]
    thread: Option<thread::JoinHandle<()>>,
    #[cfg(windows)]
    task: JoinHandle<()>,
    installed: Arc<AtomicBool>,
}

impl OwnedSessionSignalHandlers {
    /// Stops listening for process signals without killing the session.
    pub fn abort(self) {}
}

impl Drop for OwnedSessionSignalHandlers {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            self.signal_handle.close();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
        #[cfg(windows)]
        self.task.abort();
        self.installed.store(false, Ordering::Release);
    }
}

#[cfg(windows)]
async fn wait_for_default_shutdown_signal() {
    let mut waiters = ShutdownSignalWaiters::new();
    waiters.spawn(recv_ctrl_c());
    waiters.spawn(recv_windows_ctrl_break());
    waiters.spawn(recv_windows_ctrl_close());
    waiters.spawn(recv_windows_ctrl_logoff());
    waiters.spawn(recv_windows_ctrl_shutdown());
    waiters.wait().await;
}

#[cfg(all(not(unix), not(windows)))]
async fn wait_for_default_shutdown_signal() {
    std::future::pending::<()>().await;
}

#[cfg(windows)]
async fn recv_ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(windows)]
async fn recv_windows_ctrl_break() {
    let Ok(mut signal) = tokio::signal::windows::ctrl_break() else {
        std::future::pending::<()>().await;
        return;
    };
    let _ = signal.recv().await;
}

#[cfg(windows)]
async fn recv_windows_ctrl_close() {
    let Ok(mut signal) = tokio::signal::windows::ctrl_close() else {
        std::future::pending::<()>().await;
        return;
    };
    let _ = signal.recv().await;
}

#[cfg(windows)]
async fn recv_windows_ctrl_logoff() {
    let Ok(mut signal) = tokio::signal::windows::ctrl_logoff() else {
        std::future::pending::<()>().await;
        return;
    };
    let _ = signal.recv().await;
}

#[cfg(windows)]
async fn recv_windows_ctrl_shutdown() {
    let Ok(mut signal) = tokio::signal::windows::ctrl_shutdown() else {
        std::future::pending::<()>().await;
        return;
    };
    let _ = signal.recv().await;
}

#[cfg(windows)]
struct ShutdownSignalWaiters {
    sender: tokio::sync::mpsc::Sender<()>,
    receiver: tokio::sync::mpsc::Receiver<()>,
    tasks: Vec<JoinHandle<()>>,
}

#[cfg(windows)]
impl ShutdownSignalWaiters {
    fn new() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        Self {
            sender,
            receiver,
            tasks: Vec::new(),
        }
    }

    fn spawn<F>(&mut self, wait: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let sender = self.sender.clone();
        self.tasks.push(tokio::spawn(async move {
            wait.await;
            let _ = sender.try_send(());
        }));
    }

    async fn wait(mut self) {
        let _ = self.receiver.recv().await;
        for task in self.tasks {
            task.abort();
        }
    }
}
