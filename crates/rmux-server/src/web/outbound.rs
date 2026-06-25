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
const BACKLOG_BYTES_MAX: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundQueueResult {
    Queued,
    Backpressure,
    Closed,
    Full,
}

impl OutboundQueueResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Backpressure => "backpressure",
            Self::Closed => "closed",
            Self::Full => "full",
        }
    }
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
}

struct KeyframeCmd {
    frames: Vec<Vec<u8>>,
    epoch: u64,
}

impl WebSocketOutbound {
    pub(crate) fn spawn(writer: WebSocketWriter, sealer: FrameSealer) -> Self {
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
        if self.backlog_exceeds(len) {
            let result = OutboundQueueResult::Backpressure;
            trace_outbound_queue("frame", len, result, false);
            return result;
        }
        let epoch = self.latest_epoch.load(Ordering::Acquire);
        let result = match self.tx.try_send(DataCmd::Frame { bytes, epoch }) {
            Ok(()) => {
                self.backlog_bytes.fetch_add(len, Ordering::Relaxed);
                OutboundQueueResult::Queued
            }
            Err(mpsc::error::TrySendError::Closed(_)) => OutboundQueueResult::Closed,
            Err(mpsc::error::TrySendError::Full(_)) => OutboundQueueResult::Full,
        };
        trace_outbound_queue("frame", len, result, false);
        result
    }

    pub(crate) fn queue_snapshot(&self, bytes: Vec<u8>) -> OutboundQueueResult {
        let len = bytes.len();
        if self.backlog_exceeds(len) {
            let result = OutboundQueueResult::Backpressure;
            trace_outbound_queue("snapshot", len, result, false);
            return result;
        }
        let permit = match self.tx.try_reserve() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Closed(_)) => {
                let result = OutboundQueueResult::Closed;
                trace_outbound_queue("snapshot", len, result, false);
                return result;
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let result = OutboundQueueResult::Full;
                trace_outbound_queue("snapshot", len, result, false);
                return result;
            }
        };
        let epoch = self.latest_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.backlog_bytes.fetch_add(len, Ordering::Relaxed);
        permit.send(DataCmd::Snapshot { bytes, epoch });
        let result = OutboundQueueResult::Queued;
        trace_outbound_queue("snapshot", len, result, false);
        result
    }

    pub(crate) fn queue_keyframe(&self, frames: Vec<Vec<u8>>) -> OutboundQueueResult {
        let len = keyframe_len(&frames);
        if self.backlog_exceeds(len) {
            let result = OutboundQueueResult::Backpressure;
            trace_outbound_queue("keyframe", len, result, false);
            return result;
        }
        let epoch = self.latest_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        match self.latest_keyframe.lock() {
            Ok(mut latest) => {
                *latest = Some(KeyframeCmd { frames, epoch });
            }
            Err(_) => {
                let result = OutboundQueueResult::Closed;
                trace_outbound_queue("keyframe", len, result, false);
                return result;
            }
        }
        if self.keyframe_wakeup_pending.swap(true, Ordering::AcqRel) {
            let result = OutboundQueueResult::Queued;
            trace_outbound_queue("keyframe", len, result, true);
            return result;
        }
        let result = match self.control_tx.send(ControlCmd::Keyframe) {
            Ok(()) => OutboundQueueResult::Queued,
            Err(_) => {
                self.keyframe_wakeup_pending.store(false, Ordering::Release);
                OutboundQueueResult::Closed
            }
        };
        trace_outbound_queue("keyframe", len, result, false);
        result
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

    fn backlog_exceeds(&self, next_len: usize) -> bool {
        self.backlog_bytes
            .load(Ordering::Relaxed)
            .saturating_add(next_len)
            > BACKLOG_BYTES_MAX
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

async fn writer_task(
    mut writer: EncryptedWebSocketWriter,
    mut rx: mpsc::Receiver<DataCmd>,
    mut control_rx: mpsc::UnboundedReceiver<ControlCmd>,
    backlog_bytes: Arc<AtomicUsize>,
    latest_epoch: Arc<AtomicU64>,
    latest_keyframe: Arc<Mutex<Option<KeyframeCmd>>>,
    keyframe_wakeup_pending: Arc<AtomicBool>,
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
                ).await {
                    break;
                }
            }
            Some(cmd) = rx.recv() => {
                if !handle_data_cmd(
                    &mut writer,
                    cmd,
                    backlog_bytes.as_ref(),
                    latest_epoch.as_ref(),
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
    backlog_bytes: &AtomicUsize,
    latest_epoch: &AtomicU64,
) -> bool {
    match cmd {
        DataCmd::Frame { bytes, epoch } => {
            subtract_backlog(backlog_bytes, bytes.len());
            if epoch < latest_epoch.load(Ordering::Acquire) {
                return true;
            }
            let _span = crate::perf_instrument::span("web_writer")
                .with_str("frame", "output")
                .with_usize("bytes", bytes.len())
                .with_u64("epoch", epoch);
            if let Err(error) = write_with_timeout(writer.write_binary(&bytes)).await {
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
            subtract_backlog(backlog_bytes, bytes.len());
            if epoch < latest_epoch.load(Ordering::Acquire) {
                return true;
            }
            let _span = crate::perf_instrument::span("web_writer")
                .with_str("frame", "snapshot")
                .with_usize("bytes", bytes.len())
                .with_u64("epoch", epoch);
            if let Err(error) = write_with_timeout(writer.write_binary(&bytes)).await {
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
) -> bool {
    match cmd {
        ControlCmd::Keyframe => {
            keyframe_wakeup_pending.store(false, Ordering::Release);
            let keyframe = match latest_keyframe.lock() {
                Ok(mut latest) => latest.take(),
                Err(_) => None,
            };
            let Some(KeyframeCmd { frames, epoch }) = keyframe else {
                return true;
            };
            if epoch < latest_epoch.load(Ordering::Acquire) {
                return true;
            }
            for (index, frame) in frames.iter().enumerate() {
                let _span = crate::perf_instrument::span("web_writer")
                    .with_str("frame", "keyframe")
                    .with_usize("bytes", frame.len())
                    .with_usize("frame_index", index)
                    .with_usize("frame_count", frames.len())
                    .with_u64("epoch", epoch);
                if let Err(error) = write_with_timeout(writer.write_binary(frame)).await {
                    warn!(
                        frame = "keyframe",
                        epoch,
                        frame_index = index,
                        frame_count = frames.len(),
                        error = %error,
                        "web-share writer task stopped"
                    );
                    return false;
                }
            }
            latest_epoch.fetch_max(epoch, Ordering::Release);
        }
        ControlCmd::Text { text, done } => {
            let _span = crate::perf_instrument::span("web_writer")
                .with_str("frame", "text")
                .with_usize("bytes", text.len());
            let result = write_with_timeout(writer.write_text(&text)).await;
            let failed = log_writer_failure("text", &result);
            let _ = done.send(result);
            if failed {
                return false;
            }
        }
        ControlCmd::Close { code, reason, done } => {
            let _span = crate::perf_instrument::span("web_writer")
                .with_str("frame", "close")
                .with_usize("bytes", reason.len());
            let result = match code {
                Some(code) => write_with_timeout(writer.write_close_code(code, &reason)).await,
                None => write_with_timeout(writer.write_close()).await,
            };
            let failed = log_writer_failure("close", &result);
            let _ = done.send(result);
            if failed {
                return false;
            }
        }
        ControlCmd::Pong { payload, done } => {
            let _span = crate::perf_instrument::span("web_writer")
                .with_str("frame", "pong")
                .with_usize("bytes", payload.len());
            let result = write_with_timeout(writer.write_pong(&payload)).await;
            let failed = log_writer_failure("pong", &result);
            let _ = done.send(result);
            if failed {
                return false;
            }
        }
    }
    true
}

fn trace_outbound_queue(
    kind: &'static str,
    bytes: usize,
    result: OutboundQueueResult,
    coalesced: bool,
) {
    crate::perf_instrument::event("queue_backpressure")
        .with_str("queue", "web_outbound")
        .with_str("kind", kind)
        .with_usize("bytes", bytes)
        .with_str("result", result.as_str())
        .with_bool("coalesced", coalesced)
        .emit();
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{
        ControlCmd, OutboundQueueResult, WebSocketOutbound, BACKLOG_BYTES_MAX, VIEWER_CHANNEL_CAP,
    };

    #[tokio::test]
    async fn keyframe_replaces_latest_even_when_data_queue_is_full() {
        let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

        for _ in 0..VIEWER_CHANNEL_CAP {
            assert_eq!(
                outbound.queue_frame(vec![b'x']),
                OutboundQueueResult::Queued
            );
        }
        assert_eq!(outbound.queue_frame(vec![b'y']), OutboundQueueResult::Full);

        assert_eq!(
            outbound.queue_keyframe(vec![vec![b'o', b'l', b'd']]),
            OutboundQueueResult::Queued
        );
        assert_eq!(
            outbound.queue_keyframe(vec![vec![b'n', b'e', b'w']]),
            OutboundQueueResult::Queued
        );

        assert!(matches!(control_rx.try_recv(), Ok(ControlCmd::Keyframe)));
        assert!(control_rx.try_recv().is_err());
        let latest = outbound.latest_keyframe.lock().expect("keyframe lock");
        let latest = latest.as_ref().expect("latest keyframe retained");
        assert_eq!(latest.frames, vec![vec![b'n', b'e', b'w']]);
        assert_eq!(latest.epoch, outbound.latest_epoch.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn snapshots_respect_backlog_byte_limit() {
        let (outbound, _data_rx, _control_rx) = WebSocketOutbound::test_channels();

        assert_eq!(
            outbound.queue_frame(vec![0; BACKLOG_BYTES_MAX]),
            OutboundQueueResult::Queued
        );
        assert_eq!(
            outbound.queue_frame(vec![0]),
            OutboundQueueResult::Backpressure
        );
        assert_eq!(
            outbound.queue_snapshot(vec![0]),
            OutboundQueueResult::Backpressure
        );
        assert_eq!(
            outbound.backlog_bytes.load(Ordering::Acquire),
            BACKLOG_BYTES_MAX
        );
    }

    #[tokio::test]
    async fn keyframes_respect_backlog_byte_limit_without_advancing_epoch() {
        let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

        assert_eq!(
            outbound.queue_frame(vec![0; BACKLOG_BYTES_MAX]),
            OutboundQueueResult::Queued
        );
        assert_eq!(
            outbound.queue_keyframe(vec![vec![0]]),
            OutboundQueueResult::Backpressure
        );

        assert_eq!(outbound.latest_epoch.load(Ordering::Acquire), 0);
        assert!(control_rx.try_recv().is_err());
        let latest = outbound.latest_keyframe.lock().expect("keyframe lock");
        assert!(latest.is_none());
    }

    #[tokio::test]
    async fn oversized_keyframes_are_rejected_without_epoch_gap() {
        let (outbound, _data_rx, mut control_rx) = WebSocketOutbound::test_channels();

        assert_eq!(
            outbound.queue_keyframe(vec![vec![0; BACKLOG_BYTES_MAX], vec![0]]),
            OutboundQueueResult::Backpressure
        );

        assert_eq!(outbound.latest_epoch.load(Ordering::Acquire), 0);
        assert!(control_rx.try_recv().is_err());
        let latest = outbound.latest_keyframe.lock().expect("keyframe lock");
        assert!(latest.is_none());
    }
}
