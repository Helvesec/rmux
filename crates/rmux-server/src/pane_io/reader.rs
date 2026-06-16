use std::io;

use rmux_core::PaneId;
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::{PtyIo, PtyMaster};
#[cfg(unix)]
use std::sync::{Mutex, OnceLock};
#[cfg(unix)]
use std::time::{Duration, Instant};
use tracing::warn;

#[cfg(unix)]
use super::wire::{
    open_pane_writer, read_from_pane, try_read_available_from_pane, PaneReadinessState,
};
use super::{
    PaneAlertCallback, PaneAlertEvent, PaneExitCallback, PaneExitEvent, PaneOutputSender,
    READ_BUFFER_SIZE,
};
#[cfg(unix)]
use crate::pane_reader_runtime::PaneReaderRuntime;
use crate::pane_transcript::SharedPaneTranscript;

#[cfg(unix)]
const PANE_BLOCKING_PARSE_MIN_BYTES: usize = 1024 * 1024;
#[cfg(unix)]
const PANE_READ_BATCH_TRIGGER_BYTES: usize = 1;
#[cfg(unix)]
const PANE_READ_BATCH_LIMIT: usize = 64;
#[cfg(unix)]
const PANE_READ_BATCH_MAX_BYTES: usize = 4 * 1024 * 1024;
#[cfg(unix)]
// `malloc_trim` is process-global and can dominate interactive PTY latency when
// called after many tiny reads. Keep it as a coarse pressure release for real
// output volume instead of a hot-loop tax on keypress echoes.
const PANE_READ_BYTES_BEFORE_HEAP_TRIM: usize = 8 * 1024 * 1024;
#[cfg(unix)]
const PANE_HEAP_TRIM_MIN_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(unix)]
const PANE_SUSTAINED_SMALL_READ_MAX_BYTES: usize = 4096;
#[cfg(unix)]
const PANE_SUSTAINED_READ_MIN_BATCHES: u8 = 64;
#[cfg(unix)]
const PANE_SUSTAINED_READ_MIN_DURATION: Duration = Duration::from_millis(500);
#[cfg(unix)]
const PANE_ACTIVITY_ALERT_MIN_INTERVAL: Duration = Duration::from_millis(200);

#[cfg(unix)]
#[derive(Debug, Default)]
struct PaneActivityAlertThrottle {
    last_activity_alert_at: Option<std::time::Instant>,
}

#[cfg(unix)]
impl PaneActivityAlertThrottle {
    fn should_emit_no_bell_alert(&mut self) -> bool {
        self.should_emit_no_bell_alert_at(std::time::Instant::now())
    }

    fn should_emit_no_bell_alert_at(&mut self, now: std::time::Instant) -> bool {
        if self.last_activity_alert_at.is_some_and(|last| {
            now.saturating_duration_since(last) < PANE_ACTIVITY_ALERT_MIN_INTERVAL
        }) {
            return false;
        }
        self.last_activity_alert_at = Some(now);
        true
    }
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct SustainedReadCoalescer {
    burst_started_at: Option<tokio::time::Instant>,
    small_reads: u8,
}

#[cfg(unix)]
impl SustainedReadCoalescer {
    fn should_yield(&mut self, bytes_read: usize) -> bool {
        self.should_yield_at(bytes_read, tokio::time::Instant::now())
    }

    fn should_yield_at(&mut self, bytes_read: usize, now: tokio::time::Instant) -> bool {
        if bytes_read == 0 || bytes_read > PANE_SUSTAINED_SMALL_READ_MAX_BYTES {
            self.reset();
            return false;
        }

        let started_at = *self.burst_started_at.get_or_insert(now);
        self.small_reads = self
            .small_reads
            .saturating_add(1)
            .min(PANE_SUSTAINED_READ_MIN_BATCHES);

        self.small_reads >= PANE_SUSTAINED_READ_MIN_BATCHES
            && now.saturating_duration_since(started_at) >= PANE_SUSTAINED_READ_MIN_DURATION
    }

    fn reset(&mut self) {
        self.burst_started_at = None;
        self.small_reads = 0;
    }
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct PaneOutputReaderTask {
    abort: tokio::task::AbortHandle,
}

#[cfg(unix)]
impl PaneOutputReaderTask {
    pub(crate) fn abort(self) {
        self.abort.abort();
    }
}

#[cfg(unix)]
impl Drop for PaneOutputReaderTask {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct HeapTrimState {
    pending_bytes: usize,
    last_trim_at: Option<Instant>,
}

#[cfg(unix)]
fn heap_trim_state() -> &'static Mutex<HeapTrimState> {
    static STATE: OnceLock<Mutex<HeapTrimState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HeapTrimState::default()))
}

#[cfg(unix)]
fn maybe_trim_process_heap_after(bytes: usize) {
    let Ok(mut state) = heap_trim_state().try_lock() else {
        return;
    };
    state.pending_bytes = state.pending_bytes.saturating_add(bytes);
    if state.pending_bytes < PANE_READ_BYTES_BEFORE_HEAP_TRIM {
        return;
    }
    let now = Instant::now();
    if state
        .last_trim_at
        .is_some_and(|last| now.saturating_duration_since(last) < PANE_HEAP_TRIM_MIN_INTERVAL)
    {
        return;
    }
    state.pending_bytes = 0;
    state.last_trim_at = Some(now);
    drop(state);
    drop(tokio::task::spawn_blocking(
        rmux_os::memory::trim_process_heap,
    ));
}

struct PaneOutputReaderSpawn {
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    pane_master: PtyMaster,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
    #[cfg(unix)]
    runtime: PaneReaderRuntime,
}

struct PanePublishContext<'a> {
    session_name: &'a rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: &'a SharedPaneTranscript,
    pane_output: &'a PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<&'a PaneAlertCallback>,
    emit_no_bell_alert: bool,
}

struct OwnedPanePublishContext {
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    emit_no_bell_alert: bool,
}

#[allow(clippy::too_many_arguments)]
#[cfg(unix)]
pub(crate) fn spawn_pane_output_reader(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    pane_master: PtyMaster,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
    runtime: PaneReaderRuntime,
) -> PaneOutputReaderTask {
    let spawn = PaneOutputReaderSpawn {
        session_name,
        pane_id,
        pane_master,
        transcript,
        pane_output,
        generation,
        pane_alert_callback,
        pane_exit_callback,
        runtime,
    };
    spawn_async_pane_output_reader(spawn)
}

#[cfg(unix)]
fn spawn_async_pane_output_reader(spawn: PaneOutputReaderSpawn) -> PaneOutputReaderTask {
    let PaneOutputReaderSpawn {
        session_name,
        pane_id,
        pane_master,
        transcript,
        pane_output,
        generation,
        pane_alert_callback,
        pane_exit_callback,
        runtime,
    } = spawn;
    let task = async move {
        if let Err(error) = read_pane_output(
            pane_master,
            session_name.clone(),
            pane_id,
            transcript,
            pane_output,
            generation,
            pane_alert_callback,
            pane_exit_callback,
        )
        .await
        {
            warn!(
                session = %session_name,
                pane_id = pane_id.as_u32(),
                "pane output reader stopped: {error}"
            );
        }
    };
    PaneOutputReaderTask {
        abort: runtime.spawn(task),
    }
}

#[cfg(windows)]
pub(crate) fn spawn_pane_exit_watcher(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    mut child: PtyChild,
    generation: Option<u64>,
    pane_exit_callback: Option<PaneExitCallback>,
) {
    let Some(pane_exit_callback) = pane_exit_callback else {
        return;
    };
    let thread_name = format!("rmux-pane-exit-{}", pane_id.as_u32());
    let session_for_log = session_name.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            let _ = child.wait();
            child.close_pseudoconsole();
            pane_exit_callback(PaneExitEvent {
                session_name,
                pane_id,
                generation,
            });
        })
    {
        warn!(
            session = %session_for_log,
            pane_id = pane_id.as_u32(),
            thread = %thread_name,
            "failed to spawn pane exit watcher: {error}"
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(windows)]
pub(crate) fn spawn_pane_output_reader(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    pane_master: PtyMaster,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
) {
    spawn_blocking_pane_output_reader_inner(
        session_name,
        pane_id,
        pane_master,
        transcript,
        pane_output,
        generation,
        pane_alert_callback,
        pane_exit_callback,
    );
}

#[allow(clippy::too_many_arguments)]
#[cfg(unix)]
async fn read_pane_output(
    pane_master: PtyMaster,
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
) -> io::Result<()> {
    let pane_reader = open_pane_writer(pane_master)?;
    let mut buffer = vec![0_u8; READ_BUFFER_SIZE];
    let mut readiness = PaneReadinessState::default();
    let mut read_bytes_since_heap_trim = 0_usize;
    let mut sustained_reads = SustainedReadCoalescer::default();
    let mut activity_alert_throttle = PaneActivityAlertThrottle::default();

    loop {
        let bytes_read = read_from_pane(&pane_reader, &mut readiness, &mut buffer).await?;
        if bytes_read == 0 {
            if readiness.startup_eio_exhausted() {
                warn!(
                    session = %session_name,
                    pane_id = pane_id.as_u32(),
                    generation = ?generation,
                    startup_eio_reads = readiness.startup_eio_reads(),
                    "pane PTY reader exhausted startup EIO retries before first output"
                );
            }
            let _ = pane_output.send_for_generation(generation, Vec::new());
            if let Some(callback) = &pane_exit_callback {
                callback(PaneExitEvent {
                    session_name: session_name.clone(),
                    pane_id,
                    generation,
                });
            }
            return Ok(());
        }

        let sustained_small_reads = sustained_reads.should_yield(bytes_read);

        let initial_capacity = if bytes_read == buffer.len() {
            buffer.len().saturating_mul(4)
        } else {
            bytes_read
        };
        let mut bytes = Vec::with_capacity(initial_capacity);
        bytes.extend_from_slice(&buffer[..bytes_read]);
        let mut batch_reads = 1_usize;
        if bytes_read >= PANE_READ_BATCH_TRIGGER_BYTES {
            for _ in 1..PANE_READ_BATCH_LIMIT {
                match try_read_available_from_pane(&pane_reader, &mut buffer)? {
                    Some(0) | None => break,
                    Some(next_read) => {
                        batch_reads = batch_reads.saturating_add(1);
                        bytes.extend_from_slice(&buffer[..next_read]);
                        if next_read < PANE_READ_BATCH_TRIGGER_BYTES
                            || bytes.len() >= PANE_READ_BATCH_MAX_BYTES
                        {
                            break;
                        }
                    }
                }
            }
        }
        let read_saturated = batch_reads >= PANE_READ_BATCH_LIMIT;
        let published_bytes = bytes.len();
        let emit_no_bell_alert = activity_alert_throttle.should_emit_no_bell_alert();
        let replies = if bytes.len() < PANE_BLOCKING_PARSE_MIN_BYTES {
            publish_pane_bytes(
                PanePublishContext {
                    session_name: &session_name,
                    pane_id,
                    transcript: &transcript,
                    pane_output: &pane_output,
                    generation,
                    pane_alert_callback: pane_alert_callback.as_ref(),
                    emit_no_bell_alert,
                },
                bytes,
            )
        } else {
            publish_pane_bytes_on_blocking_pool(
                OwnedPanePublishContext {
                    session_name: session_name.clone(),
                    pane_id,
                    transcript: transcript.clone(),
                    pane_output: pane_output.clone(),
                    generation,
                    pane_alert_callback: pane_alert_callback.clone(),
                    emit_no_bell_alert,
                },
                bytes,
            )
            .await?
        };
        write_parser_replies_to_pane(&pane_reader, replies).await?;
        read_bytes_since_heap_trim = read_bytes_since_heap_trim.saturating_add(published_bytes);
        if read_bytes_since_heap_trim >= PANE_READ_BYTES_BEFORE_HEAP_TRIM {
            read_bytes_since_heap_trim = 0;
            maybe_trim_process_heap_after(PANE_READ_BYTES_BEFORE_HEAP_TRIM);
        }
        if read_saturated || sustained_small_reads {
            tokio::task::yield_now().await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(windows)]
fn read_pane_output_blocking(
    pane_master: PtyMaster,
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    _pane_exit_callback: Option<PaneExitCallback>,
) -> io::Result<()> {
    let pane_reader = pane_master.into_io();
    let mut buffer = vec![0_u8; READ_BUFFER_SIZE];

    loop {
        let bytes_read = match pane_reader.read(&mut buffer) {
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if bytes_read == 0 {
            let _ = pane_output.send_for_generation(generation, Vec::new());
            return Ok(());
        }

        let replies = publish_pane_bytes(
            PanePublishContext {
                session_name: &session_name,
                pane_id,
                transcript: &transcript,
                pane_output: &pane_output,
                generation,
                pane_alert_callback: pane_alert_callback.as_ref(),
                emit_no_bell_alert: true,
            },
            buffer[..bytes_read].to_vec(),
        );
        write_parser_replies_to_pane_blocking(&pane_reader, replies)?;
    }
}

fn publish_pane_bytes(context: PanePublishContext<'_>, bytes: Vec<u8>) -> Vec<u8> {
    let PanePublishContext {
        session_name,
        pane_id,
        transcript,
        pane_output,
        generation,
        pane_alert_callback,
        emit_no_bell_alert,
    } = context;
    if !pane_output.accepts_generation(generation) {
        return Vec::new();
    }
    let Some((_sequence, append_result)) =
        pane_output.publish_for_generation(generation, bytes, |bytes| {
            let mut transcript = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            let mut append_result = transcript.append_bytes_with_effects(bytes);
            let passthroughs = std::mem::take(&mut append_result.passthroughs);
            (append_result, passthroughs)
        })
    else {
        return Vec::new();
    };
    let replies = append_result.replies;
    let dropped_passthrough_count = append_result.dropped_passthrough_count;
    if dropped_passthrough_count > 0 {
        warn!(
            session = %session_name,
            pane_id = pane_id.as_u32(),
            dropped = dropped_passthrough_count,
            "dropped terminal passthrough events due to parser safety limits"
        );
    }
    if let Some(callback) = pane_alert_callback {
        callback(PaneAlertEvent {
            session_name: session_name.clone(),
            pane_id,
            bell_count: append_result.bell_count,
            queue_activity_alert: emit_no_bell_alert || append_result.bell_count > 0,
            generation,
        });
    }
    replies
}

#[cfg(unix)]
async fn publish_pane_bytes_on_blocking_pool(
    context: OwnedPanePublishContext,
    bytes: Vec<u8>,
) -> io::Result<Vec<u8>> {
    if bytes.len() < PANE_BLOCKING_PARSE_MIN_BYTES {
        return Ok(publish_pane_bytes(
            PanePublishContext {
                session_name: &context.session_name,
                pane_id: context.pane_id,
                transcript: &context.transcript,
                pane_output: &context.pane_output,
                generation: context.generation,
                pane_alert_callback: context.pane_alert_callback.as_ref(),
                emit_no_bell_alert: context.emit_no_bell_alert,
            },
            bytes,
        ));
    }

    tokio::task::spawn_blocking(move || {
        let context = PanePublishContext {
            session_name: &context.session_name,
            pane_id: context.pane_id,
            transcript: &context.transcript,
            pane_output: &context.pane_output,
            generation: context.generation,
            pane_alert_callback: context.pane_alert_callback.as_ref(),
            emit_no_bell_alert: context.emit_no_bell_alert,
        };
        publish_pane_bytes(context, bytes)
    })
    .await
    .map_err(|error| io::Error::other(format!("pane parser task failed: {error}")))
}

#[cfg(unix)]
async fn write_parser_replies_to_pane(
    pane_writer: &tokio::io::unix::AsyncFd<PtyIo>,
    replies: Vec<u8>,
) -> io::Result<()> {
    if replies.is_empty() {
        return Ok(());
    }

    let mut remaining = replies.as_slice();
    while !remaining.is_empty() {
        let mut ready = pane_writer.writable().await?;
        match ready.try_io(|inner| {
            rustix::io::write(inner.get_ref().as_fd(), remaining).map_err(io::Error::from)
        }) {
            Ok(Ok(0)) => {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0"));
            }
            Ok(Ok(bytes_written)) => remaining = &remaining[bytes_written..],
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_parser_replies_to_pane_blocking(pane_writer: &PtyIo, replies: Vec<u8>) -> io::Result<()> {
    if replies.is_empty() {
        return Ok(());
    }
    pane_writer.write_all(&replies)
}

#[allow(clippy::too_many_arguments)]
#[cfg(windows)]
fn spawn_blocking_pane_output_reader_inner(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    pane_master: PtyMaster,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
) {
    let thread_name = format!("rmux-pane-reader-{}", pane_id.as_u32());
    let session_for_log = session_name.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            if let Err(error) = read_pane_output_blocking(
                pane_master,
                session_name.clone(),
                pane_id,
                transcript,
                pane_output,
                generation,
                pane_alert_callback,
                pane_exit_callback,
            ) {
                warn!(
                    session = %session_name,
                    pane_id = pane_id.as_u32(),
                    "pane output reader stopped: {error}"
                );
            }
        })
    {
        warn!(
            session = %session_for_log,
            pane_id = pane_id.as_u32(),
            thread = %thread_name,
            "failed to spawn pane output reader: {error}"
        );
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::error::Error;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
    use rmux_proto::{SessionName, TerminalSize};
    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::{spawn_pane_output_reader, PaneOutputReaderTask};
    use crate::pane_io::pane_output_channel;
    use crate::pane_reader_runtime::PaneReaderRuntime;
    use crate::pane_transcript::PaneTranscript;

    #[test]
    fn output_reader_uses_64k_read_buffer_for_dense_pty_bursts() {
        assert_eq!(super::READ_BUFFER_SIZE, 64 * 1024);
    }

    #[test]
    fn output_reader_batches_from_first_available_byte() {
        assert_eq!(super::PANE_READ_BATCH_TRIGGER_BYTES, 1);
    }

    #[test]
    fn small_read_yield_detector_ignores_short_bursts() {
        let mut coalescer = super::SustainedReadCoalescer::default();
        let start = tokio::time::Instant::now();

        for index in 0..super::PANE_SUSTAINED_READ_MIN_BATCHES {
            assert!(
                !coalescer.should_yield_at(128, start + Duration::from_millis(u64::from(index)))
            );
        }
    }

    #[test]
    fn small_read_yield_detector_yields_after_sustained_small_output() {
        let mut coalescer = super::SustainedReadCoalescer::default();
        let start = tokio::time::Instant::now();

        for index in 0..super::PANE_SUSTAINED_READ_MIN_BATCHES - 1 {
            assert!(
                !coalescer.should_yield_at(128, start + Duration::from_millis(u64::from(index)))
            );
        }
        assert!(coalescer.should_yield_at(128, start + super::PANE_SUSTAINED_READ_MIN_DURATION));
    }

    #[test]
    fn small_read_yield_detector_resets_on_large_read() {
        let mut coalescer = super::SustainedReadCoalescer::default();
        let start = tokio::time::Instant::now();

        for index in 0..super::PANE_SUSTAINED_READ_MIN_BATCHES - 1 {
            let _ = coalescer.should_yield_at(128, start + Duration::from_millis(u64::from(index)));
        }
        assert!(coalescer.should_yield_at(128, start + super::PANE_SUSTAINED_READ_MIN_DURATION));
        assert!(!coalescer.should_yield_at(
            super::PANE_SUSTAINED_SMALL_READ_MAX_BYTES + 1,
            start + super::PANE_SUSTAINED_READ_MIN_DURATION * 2
        ));
        assert!(
            !coalescer.should_yield_at(128, start + super::PANE_SUSTAINED_READ_MIN_DURATION * 3)
        );
    }

    #[test]
    fn activity_alert_throttle_bounds_no_bell_event_rate() {
        let mut throttle = super::PaneActivityAlertThrottle::default();
        let start = Instant::now();

        assert!(throttle.should_emit_no_bell_alert_at(start));
        assert!(!throttle
            .should_emit_no_bell_alert_at(start + super::PANE_ACTIVITY_ALERT_MIN_INTERVAL / 2));
        assert!(
            throttle.should_emit_no_bell_alert_at(start + super::PANE_ACTIVITY_ALERT_MIN_INTERVAL)
        );
    }

    #[tokio::test]
    async fn output_reader_writes_terminal_replies_back_to_pane() -> Result<(), Box<dyn Error>> {
        if !python3_available() {
            eprintln!("skipping terminal reply PTY test because python3 is unavailable");
            return Ok(());
        }
        let output = unique_temp_path("terminal-reply");
        let script = r#"
import os, select, sys, termios, tty
old = termios.tcgetattr(0)
tty.setraw(0)
try:
    os.write(1, b"\x1b[c")
    ready, _, _ = select.select([0], [], [], 10.0)
    data = os.read(0, 64) if ready else b""
    with open(sys.argv[1], "wb") as output:
        output.write(data)
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
"#;
        let mut spawned = ChildCommand::new("python3")
            .args(["-c", script, &output.display().to_string()])
            .size(PtyTerminalSize::new(80, 24))
            .spawn()?;
        let output_reader = spawned.master().try_clone()?;
        let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
        let pane_output = pane_output_channel();

        let output_reader_task = spawn_pane_output_reader(
            SessionName::new("terminal-reply").expect("valid session name"),
            PaneId::new(1),
            output_reader,
            transcript,
            pane_output,
            None,
            None,
            None,
            PaneReaderRuntime::current().expect("test runtime is active"),
        );

        let contents = wait_for_file_contents(&output, Duration::from_secs(12)).await?;
        let _ = spawned.child_mut().wait();
        output_reader_task.abort();
        let _ = fs::remove_file(&output);

        assert_eq!(contents, b"\x1b[?1;2c");
        Ok(())
    }

    #[tokio::test]
    async fn async_output_reader_uses_server_runtime_when_spawned_from_temporary_runtime(
    ) -> Result<(), Box<dyn Error>> {
        let mut spawned = ChildCommand::new("sh")
            .size(PtyTerminalSize::new(80, 24))
            .spawn()?;
        let output_reader = spawned.master().try_clone()?;
        let writer = spawned.master().try_clone()?;
        let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
        let pane_output = pane_output_channel();
        let server_runtime = tokio::runtime::Handle::current();
        let transcript_for_assertion = transcript.clone();

        let output_reader_task =
            std::thread::spawn(move || -> Result<PaneOutputReaderTask, String> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    Ok(spawn_pane_output_reader(
                        SessionName::new("temporary-runtime").expect("valid session name"),
                        PaneId::new(1),
                        output_reader,
                        transcript,
                        pane_output,
                        None,
                        None,
                        None,
                        PaneReaderRuntime::from_handle(server_runtime),
                    ))
                })
            })
            .join()
            .map_err(|_| "temporary runtime thread panicked")?
            .map_err(io::Error::other)?;

        writer.write_all(b"printf RMUX_SERVER_RUNTIME_OK\\n")?;
        let captured = wait_for_transcript(
            &transcript_for_assertion,
            "RMUX_SERVER_RUNTIME_OK",
            Duration::from_secs(4),
        )
        .await;

        spawned.child().terminate_forcefully()?;
        let _ = spawned.child_mut().wait();
        output_reader_task.abort();

        assert!(
            captured.contains("RMUX_SERVER_RUNTIME_OK"),
            "expected marker in transcript, got {captured:?}"
        );
        Ok(())
    }

    fn python3_available() -> bool {
        Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rmux-pane-reader-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    async fn wait_for_file_contents(
        path: &Path,
        timeout: Duration,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            match fs::read(path) {
                Ok(contents) => return Ok(contents),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => {
                    return Err(format!("timed out waiting for {}: {error}", path.display()).into());
                }
            }
        }
    }

    async fn wait_for_transcript(
        transcript: &crate::pane_transcript::SharedPaneTranscript,
        needle: &str,
        timeout: Duration,
    ) -> String {
        let deadline = Instant::now() + timeout;
        let mut captured = String::new();
        while Instant::now() < deadline {
            captured = String::from_utf8_lossy(
                &transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
            )
            .into_owned();
            if captured.contains(needle) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        captured
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::error::Error;
    use std::time::{Duration, Instant};

    use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
    use rmux_proto::{SessionName, TerminalSize};
    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::spawn_pane_output_reader;
    use crate::pane_io::pane_output_channel;
    use crate::pane_transcript::PaneTranscript;

    #[test]
    fn windows_output_reader_updates_transcript_after_written_input() -> Result<(), Box<dyn Error>>
    {
        let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
            .args(["/D", "/K"])
            .size(PtyTerminalSize::new(100, 30))
            .spawn()?;
        let output_reader = spawned.master().try_clone()?;
        let writer = spawned.master().try_clone()?;
        let transcript = PaneTranscript::shared(
            2_000,
            TerminalSize {
                cols: 100,
                rows: 30,
            },
        );
        let pane_output = pane_output_channel();

        spawn_pane_output_reader(
            SessionName::new("alpha").expect("valid session name"),
            PaneId::new(1),
            output_reader,
            transcript.clone(),
            pane_output,
            None,
            None,
            None,
        );

        writer.write_all(b"echo RMUX_READER_OK\r\n")?;
        let captured = wait_for_transcript(&transcript, "RMUX_READER_OK", Duration::from_secs(4));

        spawned.child().terminate_forcefully()?;
        let _ = spawned.child_mut().wait()?;

        assert!(
            captured.contains("RMUX_READER_OK"),
            "expected marker in transcript, got {captured:?}"
        );
        Ok(())
    }

    fn wait_for_transcript(
        transcript: &crate::pane_transcript::SharedPaneTranscript,
        needle: &str,
        timeout: Duration,
    ) -> String {
        let deadline = Instant::now() + timeout;
        let mut captured = String::new();
        while Instant::now() < deadline {
            captured = String::from_utf8_lossy(
                &transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
            )
            .into_owned();
            if captured.contains(needle) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        captured
    }
}
