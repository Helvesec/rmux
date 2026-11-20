use std::io;
use std::sync::{Arc, Mutex as StdMutex};
#[cfg(unix)]
use std::time::Duration;

use rmux_proto::RmuxError;
use rmux_pty::PtyIo;
#[cfg(unix)]
use tokio::io::unix::AsyncFd;
#[cfg(unix)]
use tokio::time::sleep;

use super::{ActiveAttachIdentity, PopupSurface, RequestHandler};

#[cfg(unix)]
pub(super) fn spawn_popup_reader_task(
    handler: RequestHandler,
    identity: ActiveAttachIdentity,
    popup_id: u64,
    surface: Arc<StdMutex<PopupSurface>>,
    reader_fd: PtyIo,
    startup_slave: Arc<PtyIo>,
) -> Result<(), RmuxError> {
    reader_fd.set_nonblocking().map_err(|error| {
        RmuxError::Server(format!("failed to make popup pty nonblocking: {error}"))
    })?;
    // tokio 1.53.3 deprecates the safe AsyncFd constructors (RUSTSEC-2026-0386); this
    // crate forbids unsafe, and the AsyncFd owns `reader_fd`, whose fd stays open.
    #[allow(deprecated)]
    let reader = AsyncFd::new(reader_fd)
        .map_err(|error| RmuxError::Server(format!("failed to watch popup pty: {error}")))?;
    tokio::spawn(async move {
        let startup = arm_popup_reader(&reader, &startup_slave);
        drop(startup_slave);
        if !startup.is_empty() {
            surface.lock().expect("popup surface").append(&startup);
            let _ = handler.popup_reader_tick(identity, popup_id).await;
        }
        loop {
            let batch = match read_popup_batch(&reader).await {
                Ok(batch) if !batch.is_empty() => batch,
                _ => break,
            };
            surface.lock().expect("popup surface").append(&batch);
            let _ = handler.popup_reader_tick(identity, popup_id).await;
        }
        handler.popup_output_closed(identity, popup_id).await;
    });
    Ok(())
}

#[cfg(windows)]
pub(super) fn spawn_popup_reader_task(
    handler: RequestHandler,
    identity: ActiveAttachIdentity,
    popup_id: u64,
    surface: Arc<StdMutex<PopupSurface>>,
    reader: PtyIo,
    _startup_slave: Arc<PtyIo>,
) -> Result<(), RmuxError> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let bytes_read = match reader.read(&mut buffer) {
                Ok(bytes_read) => bytes_read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if bytes_read == 0 {
                break;
            }
            surface
                .lock()
                .expect("popup surface")
                .append(&buffer[..bytes_read]);
            let handler = handler.clone();
            runtime.block_on(async move {
                let _ = handler.popup_reader_tick(identity, popup_id).await;
            });
        }
    });
    Ok(())
}

#[cfg(unix)]
const MAX_BATCH_BYTES: usize = 256 * 1024;

/// Takes what the job queued before this reader was armed, then releases the
/// job's startup slave. Until then that slave keeps the job's exit from closing
/// the last slave, which Darwin follows, after about 0.6 s without a reader, by
/// discarding the queued output; from here on this reader drains it.
#[cfg(unix)]
pub(super) fn arm_popup_reader(reader: &AsyncFd<PtyIo>, startup_slave: &PtyIo) -> Vec<u8> {
    let mut startup = Vec::new();
    let mut buffer = [0_u8; 8192];
    while startup.len() < MAX_BATCH_BYTES {
        match reader.get_ref().try_read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => startup.extend_from_slice(&buffer[..count]),
        }
    }
    startup_slave.release_startup_slave_guard();
    startup
}

// A TUI frame can span several PTY reads. Bound the batching delay and byte
// budget so a noisy popup cannot starve input or keep extending its deadline.
#[cfg(unix)]
pub(super) async fn read_popup_batch(reader: &AsyncFd<PtyIo>) -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 8192];
    let first = read_async_fd(reader, &mut buffer).await?;
    if first == 0 {
        return Ok(Vec::new());
    }
    let mut batch = buffer[..first].to_vec();
    sleep(Duration::from_millis(8)).await;
    while batch.len() < MAX_BATCH_BYTES {
        let remaining = (MAX_BATCH_BYTES - batch.len()).min(buffer.len());
        match reader.get_ref().try_read(&mut buffer[..remaining]) {
            Ok(0) | Err(_) => break,
            Ok(count) => batch.extend_from_slice(&buffer[..count]),
        }
    }
    Ok(batch)
}

#[cfg(unix)]
pub(super) async fn read_async_fd(fd: &AsyncFd<PtyIo>, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut ready = fd.readable().await?;
        match ready.try_io(|inner| inner.get_ref().try_read(&mut *buffer)) {
            Ok(result) => return result,
            Err(_would_block) => continue,
        }
    }
}
