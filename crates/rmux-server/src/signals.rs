#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::thread;

#[cfg(unix)]
use signal_hook::consts::signal::{SIGCHLD, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2};
#[cfg(unix)]
use signal_hook::iterator::{Handle, Signals};
#[cfg(unix)]
use tokio::sync::mpsc;
#[cfg(unix)]
use tracing::debug;

#[cfg(unix)]
use crate::daemon::ShutdownHandle;
#[cfg(unix)]
use crate::diagnostic_log::record_shutdown_request;

#[cfg(unix)]
const SERVER_SIGNALS: [i32; 7] = [SIGHUP, SIGCHLD, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2];

#[cfg(unix)]
pub(crate) struct SignalWatcher {
    handle: Handle,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) enum ServerSignal {
    ChildChanged,
    RecreateSocket,
}

#[cfg(unix)]
impl std::fmt::Debug for SignalWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignalWatcher")
            .field("running", &self.thread.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
impl SignalWatcher {
    pub(crate) fn install(
        shutdown: ShutdownHandle,
        server_signals: mpsc::UnboundedSender<ServerSignal>,
    ) -> io::Result<Self> {
        let mut signals = Signals::new(SERVER_SIGNALS)?;
        let handle = signals.handle();
        let thread = thread::Builder::new()
            .name("rmux-server-signals".to_owned())
            .spawn(move || {
                for signal in signals.forever() {
                    match signal {
                        SIGINT | SIGTERM => {
                            debug!(signal, "server received shutdown signal");
                            record_shutdown_request(match signal {
                                SIGINT => "signal-sigint",
                                SIGTERM => "signal-sigterm",
                                _ => "signal",
                            });
                            shutdown.request_shutdown();
                            break;
                        }
                        SIGCHLD => {
                            debug!(signal, "server received child status signal");
                            let _ = server_signals.send(ServerSignal::ChildChanged);
                        }
                        SIGHUP | SIGQUIT | SIGUSR1 | SIGUSR2 => {
                            if signal == SIGUSR1 {
                                debug!(signal, "server received socket recreation signal");
                                let _ = server_signals.send(ServerSignal::RecreateSocket);
                            } else {
                                debug!(signal, "server ignored non-terminating signal");
                            }
                        }
                        _ => {}
                    }
                }
            })?;

        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

#[cfg(unix)]
impl Drop for SignalWatcher {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
