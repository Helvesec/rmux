use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::crypto::{EncryptedWebSocketWriter, FrameSealer};
use super::websocket::WebSocketWriter;

const WEB_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const VIEWER_CHANNEL_CAP: usize = 256;
const BACKLOG_BYTES_MAX: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundQueueResult {
    Queued,
    Backpressure,
    Closed,
    Full,
}

pub(crate) struct WebSocketOutbound {
    tx: mpsc::Sender<ViewerCmd>,
    writer: Arc<Mutex<EncryptedWebSocketWriter>>,
    backlog_bytes: Arc<AtomicUsize>,
    latest_epoch: Arc<AtomicU64>,
    writer_task: JoinHandle<()>,
}

enum ViewerCmd {
    Frame { bytes: Vec<u8>, epoch: u64 },
    Snapshot { bytes: Vec<u8>, epoch: u64 },
}

impl WebSocketOutbound {
    pub(crate) fn spawn(writer: WebSocketWriter, sealer: FrameSealer) -> Self {
        let (tx, rx) = mpsc::channel(VIEWER_CHANNEL_CAP);
        let writer = Arc::new(Mutex::new(EncryptedWebSocketWriter::new(writer, sealer)));
        let backlog_bytes = Arc::new(AtomicUsize::new(0));
        let latest_epoch = Arc::new(AtomicU64::new(0));
        let writer_task = tokio::spawn(writer_task(
            writer.clone(),
            rx,
            backlog_bytes.clone(),
            latest_epoch.clone(),
        ));
        Self {
            tx,
            writer,
            backlog_bytes,
            latest_epoch,
            writer_task,
        }
    }

    pub(crate) fn queue_frame(&self, bytes: Vec<u8>) -> OutboundQueueResult {
        if self.backlog_exceeds(bytes.len()) {
            return OutboundQueueResult::Backpressure;
        }
        let len = bytes.len();
        let epoch = self.latest_epoch.load(Ordering::Acquire);
        match self.tx.try_send(ViewerCmd::Frame { bytes, epoch }) {
            Ok(()) => {
                self.backlog_bytes.fetch_add(len, Ordering::Relaxed);
                OutboundQueueResult::Queued
            }
            Err(mpsc::error::TrySendError::Closed(_)) => OutboundQueueResult::Closed,
            Err(mpsc::error::TrySendError::Full(_)) => OutboundQueueResult::Full,
        }
    }

    pub(crate) fn queue_snapshot(&self, bytes: Vec<u8>) -> OutboundQueueResult {
        let len = bytes.len();
        let permit = match self.tx.try_reserve() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Closed(_)) => return OutboundQueueResult::Closed,
            Err(mpsc::error::TrySendError::Full(_)) => return OutboundQueueResult::Full,
        };
        let epoch = self.latest_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.backlog_bytes.fetch_add(len, Ordering::Relaxed);
        permit.send(ViewerCmd::Snapshot { bytes, epoch });
        OutboundQueueResult::Queued
    }

    pub(crate) async fn write_text(&self, text: &str) -> io::Result<()> {
        let mut writer = self.writer.lock().await;
        write_with_timeout(writer.write_text(text)).await
    }

    pub(crate) async fn write_close(&self) -> io::Result<()> {
        let mut writer = self.writer.lock().await;
        write_with_timeout(writer.write_close()).await
    }

    pub(crate) async fn write_close_code(&self, code: u16, reason: &str) -> io::Result<()> {
        let mut writer = self.writer.lock().await;
        write_with_timeout(writer.write_close_code(code, reason)).await
    }

    pub(crate) async fn write_pong(&self, payload: &[u8]) -> io::Result<()> {
        let mut writer = self.writer.lock().await;
        write_with_timeout(writer.write_pong(payload)).await
    }

    fn backlog_exceeds(&self, next_len: usize) -> bool {
        self.backlog_bytes
            .load(Ordering::Relaxed)
            .saturating_add(next_len)
            > BACKLOG_BYTES_MAX
    }
}

impl Drop for WebSocketOutbound {
    fn drop(&mut self) {
        self.writer_task.abort();
    }
}

async fn writer_task(
    writer: Arc<Mutex<EncryptedWebSocketWriter>>,
    mut rx: mpsc::Receiver<ViewerCmd>,
    backlog_bytes: Arc<AtomicUsize>,
    latest_epoch: Arc<AtomicU64>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            ViewerCmd::Frame { bytes, epoch } => {
                subtract_backlog(&backlog_bytes, bytes.len());
                if epoch < latest_epoch.load(Ordering::Acquire) {
                    continue;
                }
                let mut writer = writer.lock().await;
                if write_with_timeout(writer.write_binary(&bytes))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            ViewerCmd::Snapshot { bytes, epoch } => {
                subtract_backlog(&backlog_bytes, bytes.len());
                let mut writer = writer.lock().await;
                if write_with_timeout(writer.write_binary(&bytes))
                    .await
                    .is_err()
                {
                    break;
                }
                latest_epoch.fetch_max(epoch, Ordering::Release);
            }
        }
    }
}

fn subtract_backlog(backlog_bytes: &AtomicUsize, len: usize) {
    let _ = backlog_bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(len))
    });
}

async fn write_with_timeout<F>(operation: F) -> io::Result<()>
where
    F: std::future::Future<Output = io::Result<()>>,
{
    match timeout(WEB_WRITE_TIMEOUT, operation).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "web-share client write timed out",
        )),
    }
}
