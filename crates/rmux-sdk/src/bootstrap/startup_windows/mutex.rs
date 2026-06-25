use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rmux_ipc::{acquire_named_mutex, NamedMutexAcquire, NamedMutexError, NamedMutexGuard};

use super::StartupError;
use crate::bootstrap::deadline::StartupDeadline;

/// Owns a named-mutex acquisition on a dedicated OS thread for the entire
/// lifetime of the startup race.
///
/// Win32 mutexes are owned per-thread. Crossing an `await` between acquire
/// and release would land the release on whichever runtime thread happens to
/// be polling, where `ReleaseMutex` silently no-ops with `ERROR_NOT_OWNER`.
/// We dedicate a single OS thread to acquire, hold, and release, then
/// terminate. Releasing the mutex is what lets the next loser-process wake
/// up and discover the daemon the winner just started.
pub(super) struct StartupMutexHolder {
    pub(super) release: Option<mpsc::SyncSender<()>>,
    pub(super) thread: Option<JoinHandle<()>>,
}

impl StartupMutexHolder {
    pub(super) fn release(&mut self) {
        if let Some(tx) = self.release.take() {
            // Send may fail if the holder thread already exited (e.g. it
            // panicked while holding the guard); the join below surfaces
            // that, but there is no useful recovery here so we discard.
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            // Joining is bounded: the holder thread only runs the guard
            // drop after receiving the signal, which is microseconds of
            // syscall work. We discard panics for the same reason as above.
            let _ = thread.join();
        }
    }
}

impl Drop for StartupMutexHolder {
    fn drop(&mut self) {
        self.release();
    }
}

pub(super) async fn acquire_startup_mutex(
    pipe_name: &Path,
    mutex_name: &OsStr,
    deadline: StartupDeadline,
) -> Result<StartupMutexHolder, StartupError> {
    let pipe_owned = pipe_name.to_path_buf();
    let mutex_owned = mutex_name.to_owned();
    let (acquire_tx, acquire_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);

    let thread = thread::Builder::new()
        .name("rmux-startup-mutex".to_owned())
        .spawn(move || {
            let mutex_wait = deadline.requested_timeout().unwrap_or(Duration::MAX);
            let outcome = acquire_named_mutex(&mutex_owned, mutex_wait);
            match outcome {
                Ok(NamedMutexAcquire::Created(guard))
                | Ok(NamedMutexAcquire::Opened(guard))
                | Ok(NamedMutexAcquire::Abandoned(guard)) => {
                    if acquire_tx.send(Ok(())).is_err() {
                        // The async caller dropped the receiver before we
                        // reported success; release immediately so we never
                        // strand the kernel mutex.
                        drop(guard);
                        return;
                    }
                    // Block until the holder is dropped (signal sent) or the
                    // sender is dropped (channel closed). Either way, drop
                    // here releases the mutex on the same thread that won
                    // initial ownership.
                    let _ = release_rx.recv();
                    drop(guard);
                }
                Err(error) => {
                    let _ = acquire_tx.send(Err(error));
                }
            }
        })
        .map_err(|source| StartupError::Mutex {
            pipe_name: pipe_owned.clone(),
            source,
        })?;

    let acquired = acquire_rx.await.map_err(|_canceled| StartupError::Mutex {
        pipe_name: pipe_owned.clone(),
        source: io::Error::other("startup mutex thread exited before reporting an outcome"),
    })?;

    match acquired {
        Ok(()) => Ok(StartupMutexHolder {
            release: Some(release_tx),
            thread: Some(thread),
        }),
        Err(error) => {
            // Holder thread already exited via the failure branch; join is
            // immediate but worth doing to surface any join error.
            let _ = thread.join();
            Err(map_named_mutex_error(error, pipe_name, deadline))
        }
    }
}

pub(super) fn acquire_startup_mutex_blocking(
    pipe_name: &Path,
    mutex_name: &OsStr,
    deadline: StartupDeadline,
) -> Result<NamedMutexGuard, StartupError> {
    let mutex_wait = deadline.requested_timeout().unwrap_or(Duration::MAX);
    acquire_named_mutex(mutex_name, mutex_wait)
        .map(NamedMutexAcquire::into_guard)
        .map_err(|error| map_named_mutex_error(error, pipe_name, deadline))
}

fn map_named_mutex_error(
    error: NamedMutexError,
    pipe_name: &Path,
    deadline: StartupDeadline,
) -> StartupError {
    match error {
        NamedMutexError::TimedOut => StartupError::MutexTimeout {
            pipe_name: pipe_name.to_path_buf(),
            waited: deadline.requested_timeout().unwrap_or(Duration::MAX),
        },
        NamedMutexError::AccessDenied(source) => StartupError::MutexAccessDenied {
            pipe_name: pipe_name.to_path_buf(),
            source,
        },
        NamedMutexError::InvalidName { reason } => StartupError::InvalidMutexName {
            reason,
            pipe_name: pipe_name.to_path_buf(),
        },
        NamedMutexError::SecurityDescriptor(source)
        | NamedMutexError::Create(source)
        | NamedMutexError::Wait(source) => StartupError::Mutex {
            pipe_name: pipe_name.to_path_buf(),
            source,
        },
    }
}
