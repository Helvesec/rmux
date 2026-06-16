#[cfg(unix)]
use std::io;

#[cfg(unix)]
use rmux_os::signals::PolledServerSignal;
#[cfg(unix)]
use tokio::io::unix::AsyncFd;
#[cfg(unix)]
use tracing::debug;

#[cfg(unix)]
use crate::diagnostic_log::record_shutdown_request;

#[cfg(unix)]
pub(crate) struct SignalWatcher {
    wake: AsyncFd<rmux_os::signals::SignalWakeReadFd>,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct SignalWatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) enum ServerSignal {
    Shutdown(&'static str),
    ChildChanged,
    RecreateSocket,
}

#[cfg(unix)]
impl std::fmt::Debug for SignalWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignalWatcher")
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
impl SignalWatcher {
    pub(crate) fn install() -> io::Result<Self> {
        let wake = rmux_os::signals::install_server_signal_flags()?.into_read_fd();
        Ok(Self {
            wake: AsyncFd::new(wake)?,
        })
    }

    pub(crate) async fn wait(&self) -> io::Result<()> {
        loop {
            let mut guard = self.wake.readable().await?;
            let mut buffer = [0_u8; 64];
            match guard.try_io(|fd| read_wake_pipe(fd.get_ref(), &mut buffer)) {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => continue,
            }
        }
    }

    pub(crate) fn poll(&self) -> Option<ServerSignal> {
        match rmux_os::signals::poll_server_signal_flags()? {
            PolledServerSignal::Interrupt => {
                debug!(signal = "SIGINT", "server received shutdown signal");
                record_shutdown_request("signal-sigint");
                Some(ServerSignal::Shutdown("signal-sigint"))
            }
            PolledServerSignal::Terminate => {
                debug!(signal = "SIGTERM", "server received shutdown signal");
                record_shutdown_request("signal-sigterm");
                Some(ServerSignal::Shutdown("signal-sigterm"))
            }
            PolledServerSignal::ChildChanged => {
                debug!(signal = "SIGCHLD", "server received child status signal");
                Some(ServerSignal::ChildChanged)
            }
            PolledServerSignal::RecreateSocket => {
                debug!(
                    signal = "SIGUSR1",
                    "server received socket recreation signal"
                );
                Some(ServerSignal::RecreateSocket)
            }
            PolledServerSignal::Ignored => {
                debug!("server ignored non-terminating signal");
                None
            }
        }
    }
}

#[cfg(unix)]
fn read_wake_pipe(fd: &rmux_os::signals::SignalWakeReadFd, buffer: &mut [u8]) -> io::Result<usize> {
    rustix::io::read(fd, buffer).map_err(io::Error::from)
}

#[cfg(not(unix))]
impl SignalWatcher {
    pub(crate) fn poll(&self) -> Option<ServerSignal> {
        None
    }
}
