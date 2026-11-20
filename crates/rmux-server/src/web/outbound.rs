use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::warn;

use super::crypto::{EncryptedWebSocketWriter, FrameSealer};
use super::websocket::WebSocketWriter;

const WEB_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const VIEWER_CHANNEL_CAP: usize = 256;
pub(crate) const WEB_OUTBOUND_BYTES_MAX: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundQueueResult {
    Queued,
    Backpressure,
    Closed,
    Full,
}

pub(crate) struct WebSocketOutbound {
    tx: mpsc::Sender<DataCmd>,
    control_tx: mpsc::UnboundedSender<ControlCmd>,
    backlog_bytes: Arc<AtomicUsize>,
    latest_epoch: Arc<AtomicU64>,
    latest_keyframe: Arc<Mutex<Option<KeyframeCmd>>>,
    keyframe_wakeup_pending: Arc<AtomicBool>,
    writer_task: JoinHandle<()>,
}

enum DataCmd {
    Frame { bytes: Vec<u8>, epoch: u64 },
    Snapshot { bytes: Vec<u8>, epoch: u64 },
}

enum ControlCmd {
    Keyframe,
    Text {
        text: String,
        done: oneshot::Sender<io::Result<()>>,
    },
    Close {
        code: Option<u16>,
        reason: String,
        done: oneshot::Sender<io::Result<()>>,
    },
    Pong {
        payload: Vec<u8>,
        done: oneshot::Sender<io::Result<()>>,
    },
    Ping {
        payload: Vec<u8>,
        done: oneshot::Sender<io::Result<()>>,
    },
}

struct KeyframeCmd {
    frames: Vec<Vec<u8>>,
    epoch: u64,
    backlog: AccountedBacklog,
}

#[derive(Debug)]
struct AccountedBacklog {
    backlog_bytes: Arc<AtomicUsize>,
    len: usize,
}

impl AccountedBacklog {
    fn replace(
        backlog_bytes: Arc<AtomicUsize>,
        replaced_len: usize,
        len: usize,
    ) -> Result<Self, OutboundQueueResult> {
        backlog_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current
                    .checked_sub(replaced_len)?
                    .checked_add(len)
                    .filter(|next| *next <= WEB_OUTBOUND_BYTES_MAX)
            })
            .map_err(|_| OutboundQueueResult::Backpressure)?;
        Ok(Self { backlog_bytes, len })
    }

    fn disarm(&mut self) {
        self.len = 0;
    }
}

impl Drop for AccountedBacklog {
    fn drop(&mut self) {
        subtract_backlog(self.backlog_bytes.as_ref(), self.len);
    }
}

struct AccountedDataPermit<'a> {
    permit: Option<mpsc::Permit<'a, DataCmd>>,
    backlog_bytes: &'a AtomicUsize,
    len: usize,
}

impl AccountedDataPermit<'_> {
    fn send(mut self, command: DataCmd) {
        self.permit
            .take()
            .expect("accounted data permit must remain available")
            .send(command);
    }
}

impl Drop for AccountedDataPermit<'_> {
    fn drop(&mut self) {
        if self.permit.is_some() {
            subtract_backlog(self.backlog_bytes, self.len);
        }
    }
}

impl WebSocketOutbound {
    pub(super) fn spawn(writer: WebSocketWriter, sealer: FrameSealer) -> Self {
        Self::spawn_with_write_timeout(writer, sealer, WEB_WRITE_TIMEOUT)
    }

    // Every wire write of this connection, data or control, must finish within
    // `write_timeout` or the writer stops. Production always passes
    // WEB_WRITE_TIMEOUT.
    fn spawn_with_write_timeout(
        writer: WebSocketWriter,
        sealer: FrameSealer,
        write_timeout: Duration,
    ) -> Self {
        let (tx, rx) = mpsc::channel(VIEWER_CHANNEL_CAP);
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let writer = EncryptedWebSocketWriter::new(writer, sealer);
        let backlog_bytes = Arc::new(AtomicUsize::new(0));
        let latest_epoch = Arc::new(AtomicU64::new(0));
        let latest_keyframe = Arc::new(Mutex::new(None));
        let keyframe_wakeup_pending = Arc::new(AtomicBool::new(false));
        let writer_task = tokio::spawn(writer_task(
            writer,
            rx,
            control_rx,
            backlog_bytes.clone(),
            latest_epoch.clone(),
            latest_keyframe.clone(),
            keyframe_wakeup_pending.clone(),
            write_timeout,
        ));
        Self {
            tx,
            control_tx,
            backlog_bytes,
            latest_epoch,
            latest_keyframe,
            keyframe_wakeup_pending,
            writer_task,
        }
    }

    pub(crate) fn queue_frame(&self, bytes: Vec<u8>) -> OutboundQueueResult {
        let len = bytes.len();
        let permit = match self.reserve_data_slot(len) {
            Ok(permit) => permit,
            Err(result) => {
                return result;
            }
        };
        let epoch = self.latest_epoch.load(Ordering::Acquire);
        permit.send(DataCmd::Frame { bytes, epoch });
        OutboundQueueResult::Queued
    }

    pub(crate) fn queue_snapshot(&self, bytes: Vec<u8>) -> OutboundQueueResult {
        let len = bytes.len();
        let permit = match self.reserve_data_slot(len) {
            Ok(permit) => permit,
            Err(result) => {
                return result;
            }
        };
        let epoch = self.latest_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        permit.send(DataCmd::Snapshot { bytes, epoch });
        OutboundQueueResult::Queued
    }

    pub(crate) fn queue_keyframe(&self, frames: Vec<Vec<u8>>) -> OutboundQueueResult {
        let len = keyframe_len(&frames);
        let mut latest = match self.latest_keyframe.lock() {
            Ok(latest) => latest,
            Err(_) => {
                return OutboundQueueResult::Closed;
            }
        };
        let replaced_len = latest.as_ref().map_or(0, |keyframe| keyframe.backlog.len);
        let backlog = match AccountedBacklog::replace(self.backlog_bytes.clone(), replaced_len, len)
        {
            Ok(backlog) => backlog,
            Err(result) => {
                return result;
            }
        };
        if let Some(replaced) = latest.as_mut() {
            replaced.backlog.disarm();
        }
        let epoch = self.latest_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        *latest = Some(KeyframeCmd {
            frames,
            epoch,
            backlog,
        });
        drop(latest);
        if self.keyframe_wakeup_pending.swap(true, Ordering::AcqRel) {
            return OutboundQueueResult::Queued;
        }
        match self.control_tx.send(ControlCmd::Keyframe) {
            Ok(()) => OutboundQueueResult::Queued,
            Err(_) => {
                self.keyframe_wakeup_pending.store(false, Ordering::Release);
                OutboundQueueResult::Closed
            }
        }
    }

    pub(crate) async fn write_text(&self, text: &str) -> io::Result<()> {
        self.enqueue_control(|done| ControlCmd::Text {
            text: text.to_owned(),
            done,
        })
        .await
    }

    pub(crate) async fn write_close(&self) -> io::Result<()> {
        self.enqueue_control(|done| ControlCmd::Close {
            code: None,
            reason: String::new(),
            done,
        })
        .await
    }

    pub(crate) async fn write_close_code(&self, code: u16, reason: &str) -> io::Result<()> {
        self.enqueue_control(|done| ControlCmd::Close {
            code: Some(code),
            reason: reason.to_owned(),
            done,
        })
        .await
    }

    pub(crate) async fn write_pong(&self, payload: &[u8]) -> io::Result<()> {
        self.enqueue_control(|done| ControlCmd::Pong {
            payload: payload.to_vec(),
            done,
        })
        .await
    }

    pub(crate) async fn write_ping(&self, payload: &[u8]) -> io::Result<()> {
        self.enqueue_control(|done| ControlCmd::Ping {
            payload: payload.to_vec(),
            done,
        })
        .await
    }

    fn reserve_data_slot(
        &self,
        len: usize,
    ) -> Result<AccountedDataPermit<'_>, OutboundQueueResult> {
        let permit = self.tx.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Closed(_) => OutboundQueueResult::Closed,
            mpsc::error::TrySendError::Full(_) => OutboundQueueResult::Full,
        })?;
        if self
            .backlog_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current
                    .checked_add(len)
                    .filter(|next| *next <= WEB_OUTBOUND_BYTES_MAX)
            })
            .is_err()
        {
            return Err(OutboundQueueResult::Backpressure);
        }
        Ok(AccountedDataPermit {
            permit: Some(permit),
            backlog_bytes: self.backlog_bytes.as_ref(),
            len,
        })
    }

    async fn enqueue_control(
        &self,
        build: impl FnOnce(oneshot::Sender<io::Result<()>>) -> ControlCmd,
    ) -> io::Result<()> {
        let (done, result) = oneshot::channel();
        self.control_tx
            .send(build(done))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "web-share writer closed"))?;
        result
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "web-share writer closed"))?
    }

    #[cfg(test)]
    pub(crate) fn stalled_for_test() -> (Self, impl Sized) {
        let (outbound, data, control) = Self::test_channels();
        (outbound, (data, control))
    }

    #[cfg(test)]
    fn test_channels() -> (
        Self,
        mpsc::Receiver<DataCmd>,
        mpsc::UnboundedReceiver<ControlCmd>,
    ) {
        let (tx, rx) = mpsc::channel(VIEWER_CHANNEL_CAP);
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let backlog_bytes = Arc::new(AtomicUsize::new(0));
        let latest_epoch = Arc::new(AtomicU64::new(0));
        let latest_keyframe = Arc::new(Mutex::new(None));
        let keyframe_wakeup_pending = Arc::new(AtomicBool::new(false));
        let writer_task = tokio::spawn(async { std::future::pending::<()>().await });
        (
            Self {
                tx,
                control_tx,
                backlog_bytes,
                latest_epoch,
                latest_keyframe,
                keyframe_wakeup_pending,
                writer_task,
            },
            rx,
            control_rx,
        )
    }
}

impl Drop for WebSocketOutbound {
    fn drop(&mut self) {
        self.writer_task.abort();
    }
}

fn keyframe_len(frames: &[Vec<u8>]) -> usize {
    frames
        .iter()
        .fold(0usize, |total, frame| total.saturating_add(frame.len()))
}

#[allow(clippy::too_many_arguments)]
async fn writer_task(
    mut writer: EncryptedWebSocketWriter,
    mut rx: mpsc::Receiver<DataCmd>,
    mut control_rx: mpsc::UnboundedReceiver<ControlCmd>,
    backlog_bytes: Arc<AtomicUsize>,
    latest_epoch: Arc<AtomicU64>,
    latest_keyframe: Arc<Mutex<Option<KeyframeCmd>>>,
    keyframe_wakeup_pending: Arc<AtomicBool>,
    write_timeout: Duration,
) {
    loop {
        tokio::select! {
            biased;
            Some(cmd) = control_rx.recv() => {
                if !handle_control_cmd(
                    &mut writer,
                    cmd,
                    latest_epoch.as_ref(),
                    latest_keyframe.as_ref(),
                    keyframe_wakeup_pending.as_ref(),
                    write_timeout,
                ).await {
                    break;
                }
            }
            Some(cmd) = rx.recv() => {
                if !handle_data_cmd(
                    &mut writer,
                    cmd,
                    &backlog_bytes,
                    latest_epoch.as_ref(),
                    write_timeout,
                ).await {
                    break;
                }
            }
            else => break,
        }
    }
}

async fn handle_data_cmd(
    writer: &mut EncryptedWebSocketWriter,
    cmd: DataCmd,
    backlog_bytes: &Arc<AtomicUsize>,
    latest_epoch: &AtomicU64,
    write_timeout: Duration,
) -> bool {
    // Dequeuing transfers ownership to the writer; it does not drain the
    // connection. Keep the charge until success, error, timeout or cancellation.
    let len = match &cmd {
        DataCmd::Frame { bytes, .. } | DataCmd::Snapshot { bytes, .. } => bytes.len(),
    };
    let _in_flight = AccountedBacklog {
        backlog_bytes: backlog_bytes.clone(),
        len,
    };
    match cmd {
        DataCmd::Frame { bytes, epoch } => {
            if epoch < latest_epoch.load(Ordering::Acquire) {
                return true;
            }
            if let Err(error) = write_with_timeout(write_timeout, writer.write_binary(&bytes)).await
            {
                warn!(
                    frame = "output",
                    epoch,
                    error = %error,
                    "web-share writer task stopped"
                );
                return false;
            }
        }
        DataCmd::Snapshot { bytes, epoch } => {
            if epoch < latest_epoch.load(Ordering::Acquire) {
                return true;
            }
            if let Err(error) = write_with_timeout(write_timeout, writer.write_binary(&bytes)).await
            {
                warn!(
                    frame = "snapshot",
                    epoch,
                    error = %error,
                    "web-share writer task stopped"
                );
                return false;
            }
            latest_epoch.fetch_max(epoch, Ordering::Release);
        }
    }
    true
}

async fn handle_control_cmd(
    writer: &mut EncryptedWebSocketWriter,
    cmd: ControlCmd,
    latest_epoch: &AtomicU64,
    latest_keyframe: &Mutex<Option<KeyframeCmd>>,
    keyframe_wakeup_pending: &AtomicBool,
    write_timeout: Duration,
) -> bool {
    match cmd {
        ControlCmd::Keyframe => {
            keyframe_wakeup_pending.store(false, Ordering::Release);
            let keyframe = match latest_keyframe.lock() {
                Ok(mut latest) => latest.take(),
                Err(_) => None,
            };
            let Some(keyframe) = keyframe else {
                return true;
            };
            if keyframe.epoch < latest_epoch.load(Ordering::Acquire) {
                return true;
            }
            for (index, frame) in keyframe.frames.iter().enumerate() {
                if let Err(error) =
                    write_with_timeout(write_timeout, writer.write_binary(frame)).await
                {
                    warn!(
                        frame = "keyframe",
                        epoch = keyframe.epoch,
                        frame_index = index,
                        frame_count = keyframe.frames.len(),
                        error = %error,
                        "web-share writer task stopped"
                    );
                    return false;
                }
            }
            latest_epoch.fetch_max(keyframe.epoch, Ordering::Release);
        }
        ControlCmd::Text { text, done } => {
            let result = write_with_timeout(write_timeout, writer.write_text(&text)).await;
            let failed = log_writer_failure("text", &result);
            let _ = done.send(result);
            if failed {
                return false;
            }
        }
        ControlCmd::Close { code, reason, done } => {
            let result = match code {
                Some(code) => {
                    write_with_timeout(write_timeout, writer.write_close_code(code, &reason)).await
                }
                None => write_with_timeout(write_timeout, writer.write_close()).await,
            };
            let failed = log_writer_failure("close", &result);
            let _ = done.send(result);
            if failed {
                return false;
            }
        }
        ControlCmd::Pong { payload, done } => {
            let result = write_with_timeout(write_timeout, writer.write_pong(&payload)).await;
            let failed = log_writer_failure("pong", &result);
            let _ = done.send(result);
            if failed {
                return false;
            }
        }
        ControlCmd::Ping { payload, done } => {
            let result = write_with_timeout(write_timeout, writer.write_ping(&payload)).await;
            let failed = log_writer_failure("ping", &result);
            let _ = done.send(result);
            if failed {
                return false;
            }
        }
    }
    true
}

fn log_writer_failure(frame: &'static str, result: &io::Result<()>) -> bool {
    if let Err(error) = result {
        warn!(
            frame,
            error = %error,
            "web-share writer task stopped"
        );
        true
    } else {
        false
    }
}

fn subtract_backlog(backlog_bytes: &AtomicUsize, len: usize) {
    let _ = backlog_bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(len))
    });
}

async fn write_with_timeout<F>(write_timeout: Duration, operation: F) -> io::Result<()>
where
    F: std::future::Future<Output = io::Result<()>>,
{
    match timeout(write_timeout, operation).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "web-share client write timed out",
        )),
    }
}

#[cfg(test)]
#[path = "outbound/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "outbound/slow_reader_tests.rs"]
mod slow_reader_tests;
