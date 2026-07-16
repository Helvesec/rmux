//! Opt-in process signal listener for owned sessions.

#[cfg(windows)]
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(unix)]
use std::thread;

#[cfg(windows)]
use tokio::task::JoinHandle;

use crate::transport::TransportClient;
use crate::{Result, RmuxError};
use rmux_proto::Request;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SignalHandlerPhase {
    #[default]
    Idle,
    Armed,
    Firing,
    Finished,
    Disarmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalHandlerDropAction {
    CancelListener,
    KeepCleanup,
}

#[derive(Debug, Default)]
pub(super) struct SignalHandlerState {
    phase: Mutex<SignalHandlerPhase>,
}

impl SignalHandlerState {
    fn reserve_install(&self) -> Result<()> {
        let mut phase = self.lock_phase();
        if *phase != SignalHandlerPhase::Idle {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "owned session signal handlers are already installed".to_owned(),
            )));
        }
        *phase = SignalHandlerPhase::Armed;
        Ok(())
    }

    pub(super) fn disarm_for_ownership_release(&self) -> Result<()> {
        let mut phase = self.lock_phase();
        match *phase {
            SignalHandlerPhase::Idle | SignalHandlerPhase::Disarmed => Ok(()),
            SignalHandlerPhase::Armed => {
                *phase = SignalHandlerPhase::Disarmed;
                Ok(())
            }
            SignalHandlerPhase::Firing => Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "owned-session signal cleanup is already in progress".to_owned(),
            ))),
            SignalHandlerPhase::Finished => {
                Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "owned-session signal cleanup has already finished".to_owned(),
                )))
            }
        }
    }

    pub(super) fn try_begin_cleanup(&self) -> bool {
        let mut phase = self.lock_phase();
        if *phase != SignalHandlerPhase::Armed {
            return false;
        }
        *phase = SignalHandlerPhase::Firing;
        true
    }

    fn finish_cleanup(&self) {
        let mut phase = self.lock_phase();
        if *phase == SignalHandlerPhase::Firing {
            *phase = SignalHandlerPhase::Finished;
        }
    }

    fn rollback_install(&self) {
        let mut phase = self.lock_phase();
        if *phase == SignalHandlerPhase::Armed {
            *phase = SignalHandlerPhase::Idle;
        }
    }

    fn release_guard(&self) -> SignalHandlerDropAction {
        let mut phase = self.lock_phase();
        match *phase {
            SignalHandlerPhase::Idle => SignalHandlerDropAction::CancelListener,
            SignalHandlerPhase::Armed | SignalHandlerPhase::Disarmed => {
                *phase = SignalHandlerPhase::Idle;
                SignalHandlerDropAction::CancelListener
            }
            SignalHandlerPhase::Firing | SignalHandlerPhase::Finished => {
                SignalHandlerDropAction::KeepCleanup
            }
        }
    }

    #[cfg(test)]
    pub(super) fn is_installed(&self) -> bool {
        *self.lock_phase() != SignalHandlerPhase::Idle
    }

    #[cfg(test)]
    pub(super) fn is_disarmed(&self) -> bool {
        *self.lock_phase() == SignalHandlerPhase::Disarmed
    }

    fn lock_phase(&self) -> std::sync::MutexGuard<'_, SignalHandlerPhase> {
        self.phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(super) fn install_default_signal_handlers(
    transport: TransportClient,
    cleanup_request: Request,
    state: Arc<SignalHandlerState>,
) -> Result<OwnedSessionSignalHandlers> {
    let reservation = SignalHandlerInstallReservation::acquire(state)?;
    #[cfg(unix)]
    let handlers =
        install_unix_signal_handlers(transport, cleanup_request, Arc::clone(&reservation.state))?;
    #[cfg(windows)]
    let handlers =
        install_tokio_signal_handlers(transport, cleanup_request, Arc::clone(&reservation.state))?;
    #[cfg(all(not(unix), not(windows)))]
    let handlers = {
        let _ = (transport, cleanup_request);
        OwnedSessionSignalHandlers {
            state: Arc::clone(&reservation.state),
        }
    };
    reservation.commit();
    Ok(handlers)
}

struct SignalHandlerInstallReservation {
    state: Arc<SignalHandlerState>,
    committed: bool,
}

impl SignalHandlerInstallReservation {
    fn acquire(state: Arc<SignalHandlerState>) -> Result<Self> {
        state.reserve_install()?;
        Ok(Self {
            state,
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SignalHandlerInstallReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.state.rollback_install();
        }
    }
}

#[cfg(windows)]
fn install_tokio_signal_handlers(
    transport: TransportClient,
    cleanup_request: Request,
    state: Arc<SignalHandlerState>,
) -> Result<OwnedSessionSignalHandlers> {
    let runtime = current_runtime()?;
    let cleanup_state = Arc::clone(&state);
    let task = runtime.spawn(async move {
        wait_for_default_shutdown_signal().await;
        if cleanup_state.try_begin_cleanup() {
            run_signal_cleanup(transport, cleanup_request, cleanup_state).await;
        }
    });

    Ok(OwnedSessionSignalHandlers { task, state })
}

#[cfg(unix)]
fn install_unix_signal_handlers(
    transport: TransportClient,
    cleanup_request: Request,
    state: Arc<SignalHandlerState>,
) -> Result<OwnedSessionSignalHandlers> {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let runtime = current_runtime()?;
    let mut signals =
        Signals::new([SIGINT, SIGTERM, SIGHUP]).map_err(|source| RmuxError::Transport {
            operation: "install owned-session signal handlers".to_owned(),
            source,
        })?;
    let handle = signals.handle();
    let cleanup_state = Arc::clone(&state);
    let thread = thread::Builder::new()
        .name("rmux-sdk-owned-signals".to_owned())
        .spawn(move || {
            if signals.forever().next().is_some() && cleanup_state.try_begin_cleanup() {
                runtime.spawn(run_signal_cleanup(
                    transport,
                    cleanup_request,
                    cleanup_state,
                ));
            }
        })
        .map_err(|source| RmuxError::Transport {
            operation: "start owned-session signal handler".to_owned(),
            source,
        })?;

    Ok(OwnedSessionSignalHandlers {
        signal_handle: handle,
        thread: Some(thread),
        state,
    })
}

#[cfg(any(unix, windows))]
async fn run_signal_cleanup(
    transport: TransportClient,
    cleanup_request: Request,
    state: Arc<SignalHandlerState>,
) {
    let _ = transport.request(cleanup_request).await;
    state.finish_cleanup();
}

#[cfg(any(unix, windows))]
fn current_runtime() -> Result<tokio::runtime::Handle> {
    tokio::runtime::Handle::try_current().map_err(|error| {
        RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "owned session signal handlers require a Tokio runtime: {error}"
        )))
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
    state: Arc<SignalHandlerState>,
}

impl OwnedSessionSignalHandlers {
    /// Stops listening for process signals without starting cleanup.
    /// Cleanup that already started is allowed to finish.
    pub fn abort(self) {}
}

impl Drop for OwnedSessionSignalHandlers {
    fn drop(&mut self) {
        let drop_action = self.state.release_guard();
        #[cfg(unix)]
        {
            self.signal_handle.close();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
        #[cfg(windows)]
        if drop_action == SignalHandlerDropAction::CancelListener {
            self.task.abort();
        }
        #[cfg(not(windows))]
        let _ = drop_action;
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

#[cfg(test)]
mod phase_tests {
    #[cfg(any(unix, windows))]
    use std::sync::Arc;

    #[cfg(any(unix, windows))]
    use super::run_signal_cleanup;
    use super::{SignalHandlerDropAction, SignalHandlerState};
    #[cfg(any(unix, windows))]
    use crate::transport::TransportClient;

    #[test]
    fn armed_signal_guard_releases_the_installation_reservation_on_drop() {
        let state = SignalHandlerState::default();
        state
            .reserve_install()
            .expect("first install reserves state");

        assert_eq!(
            state.release_guard(),
            SignalHandlerDropAction::CancelListener
        );

        state
            .reserve_install()
            .expect("dropping an armed guard must permit a later installation");
    }

    #[test]
    fn in_flight_cleanup_survives_guard_drop_and_stays_fail_closed() {
        let state = SignalHandlerState::default();
        state
            .reserve_install()
            .expect("first install reserves state");
        assert!(state.try_begin_cleanup(), "signal begins cleanup once");

        assert_eq!(state.release_guard(), SignalHandlerDropAction::KeepCleanup);
        assert!(
            state.reserve_install().is_err(),
            "guard drop must not make an in-flight cleanup installable again"
        );
        let error = state
            .disarm_for_ownership_release()
            .expect_err("canceled or in-flight cleanup must block ownership release");
        assert!(
            error.to_string().contains("already in progress"),
            "unexpected ownership-release error: {error}"
        );
    }

    #[test]
    fn finished_cleanup_remains_terminal_after_guard_drop() {
        let state = SignalHandlerState::default();
        state
            .reserve_install()
            .expect("first install reserves state");
        assert!(state.try_begin_cleanup(), "signal begins cleanup once");
        state.finish_cleanup();

        assert_eq!(state.release_guard(), SignalHandlerDropAction::KeepCleanup);
        assert!(
            state.reserve_install().is_err(),
            "completed signal cleanup must not allow another installation"
        );
        let error = state
            .disarm_for_ownership_release()
            .expect_err("completed signal cleanup must block ownership release");
        assert!(
            error.to_string().contains("already finished"),
            "unexpected ownership-release error: {error}"
        );
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn completed_cleanup_request_marks_the_state_terminal() {
        let (client_stream, server_stream) = tokio::io::duplex(1024);
        drop(server_stream);
        let state = Arc::new(SignalHandlerState::default());
        state
            .reserve_install()
            .expect("first install reserves state");
        assert!(state.try_begin_cleanup(), "signal begins cleanup once");

        run_signal_cleanup(
            TransportClient::spawn(client_stream),
            super::super::session_identity_kill_request(crate::SessionId::new(42)),
            Arc::clone(&state),
        )
        .await;

        let error = state
            .disarm_for_ownership_release()
            .expect_err("a completed cleanup request must block ownership release");
        assert!(
            error.to_string().contains("already finished"),
            "unexpected ownership-release error: {error}"
        );
    }
}
