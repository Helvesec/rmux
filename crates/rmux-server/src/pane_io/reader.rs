use std::io;

use rmux_core::{PaneId, TerminalPassthrough, TerminalPassthroughKind};
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::{PtyIo, PtyMaster};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Mutex;
use std::sync::{mpsc as std_mpsc, Arc, OnceLock};
#[cfg(windows)]
use std::time::{Duration, Instant};
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
use crate::clipboard_protocol::decode_pane_clipboard_write_payload;
#[cfg(unix)]
use crate::pane_reader_runtime::PaneReaderRuntime;
use crate::pane_transcript::{PaneGroundTimer, SharedPaneTranscript};

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
#[cfg(windows)]
const WINDOWS_PANE_EOF_PUBLISHED_GRACE: Duration = Duration::from_millis(25);
#[cfg(windows)]
const WINDOWS_PANE_EOF_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[cfg(windows)]
#[derive(Clone, Debug, Default)]
pub(crate) struct PaneOutputEofState {
    published: Arc<AtomicBool>,
}

#[cfg(windows)]
impl PaneOutputEofState {
    fn mark_published(&self) {
        self.published.store(true, Ordering::Release);
    }

    fn is_published(&self) -> bool {
        self.published.load(Ordering::Acquire)
    }

    fn wait_until_published(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_published() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return self.is_published();
            }
            std::thread::sleep((deadline - now).min(WINDOWS_PANE_EOF_POLL_INTERVAL));
        }
    }
}

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
    eof_state: PaneOutputEofState,
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
            if let Err(error) = child.terminate_forcefully() {
                warn!(
                    session = %session_name,
                    pane_id = pane_id.as_u32(),
                    "failed to terminate pane descendants before closing ConPTY: {error}"
                );
                child.close_pseudoconsole();
            }
            if eof_state.wait_until_published(WINDOWS_PANE_EOF_PUBLISHED_GRACE) {
                return;
            }
            pane_exit_callback(PaneExitEvent::eof_pending(
                session_name,
                pane_id,
                generation,
            ));
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
    eof_state: PaneOutputEofState,
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
        eof_state,
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
                callback(PaneExitEvent::eof_published(
                    session_name.clone(),
                    pane_id,
                    generation,
                ));
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
    eof_state: PaneOutputEofState,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
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
            eof_state.mark_published();
            if let Some(callback) = &pane_exit_callback {
                callback(PaneExitEvent::eof_published(
                    session_name.clone(),
                    pane_id,
                    generation,
                ));
            }
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
            let mouse_mode_before = transcript.mode() & rmux_core::input::mode::ALL_MOUSE_MODES;
            let mut append_result = transcript.append_bytes_with_effects(bytes);
            let mouse_mode_changed =
                transcript.mode() & rmux_core::input::mode::ALL_MOUSE_MODES != mouse_mode_before;
            let passthroughs = std::mem::take(&mut append_result.passthroughs);
            let clipboard_set = passthroughs.iter().any(passthrough_is_clipboard_set);
            let clipboard_writes = passthroughs
                .iter()
                .filter_map(osc52_clipboard_write_payload)
                .collect::<Vec<_>>();
            let clipboard_queries = passthroughs
                .iter()
                .filter_map(TerminalPassthrough::clipboard_query_metadata)
                .collect::<Vec<_>>();
            if let Some(callback) = pane_alert_callback {
                callback(PaneAlertEvent {
                    session_name: session_name.clone(),
                    pane_id,
                    bell_count: append_result.bell_count,
                    title_changed: append_result.title_changed,
                    title_change: append_result.title_change.clone(),
                    clipboard_set,
                    clipboard_writes,
                    clipboard_queries,
                    mouse_mode_changed,
                    alternate_mode_changed: append_result.alternate_mode_changed,
                    queue_activity_alert: emit_no_bell_alert || append_result.bell_count > 0,
                    generation,
                });
            }
            (append_result, passthroughs)
        })
    else {
        return Vec::new();
    };
    if let Some(timer) = append_result.ground_timer {
        schedule_pane_ground_timer(session_name, pane_id, Arc::clone(transcript), timer);
    }
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
    replies
}

fn passthrough_is_clipboard_set(passthrough: &TerminalPassthrough) -> bool {
    passthrough.kind() == TerminalPassthroughKind::Clipboard
        && osc52_payload_is_clipboard_set(passthrough.payload())
}

fn osc52_payload_is_clipboard_set(sequence: &[u8]) -> bool {
    let Some(body) = sequence.strip_prefix(b"\x1b]52;") else {
        return false;
    };
    let Some(body) = body
        .strip_suffix(b"\x07")
        .or_else(|| body.strip_suffix(b"\x1b\\"))
    else {
        return false;
    };
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return false;
    };
    let payload = &body[separator + 1..];
    if payload.is_empty() || payload == b"?" {
        return false;
    }
    // Use the same strict decoder as `osc52_clipboard_write_payload` so the
    // clipboard_set flag (which drives the PaneSetClipboard hook emission)
    // stays in sync with buffer storage. Base64 payloads that decode to zero
    // bytes (e.g. a lone `==` sequence) match the syntax-only classifier but
    // are dropped by paste_add on the frozen tmux 3.7b oracle — mirror that
    // so the hook does not fire without a stored buffer.
    osc52_payload_decodes(payload)
}

/// Decodes an inbound OSC 52 clipboard-write passthrough to its raw bytes, or
/// returns None for a query (`?`), an empty/malformed payload, or a
/// non-clipboard passthrough. Matches the frozen tmux 3.7b oracle: empty and
/// invalid-base64 writes create no paste buffer (input_osc_52 returns early
/// without paste_add).
fn osc52_clipboard_write_payload(passthrough: &TerminalPassthrough) -> Option<Vec<u8>> {
    if passthrough.kind() != TerminalPassthroughKind::Clipboard {
        return None;
    }
    let body = passthrough.payload().strip_prefix(b"\x1b]52;")?;
    let body = body
        .strip_suffix(b"\x07")
        .or_else(|| body.strip_suffix(b"\x1b\\"))?;
    let separator = body.iter().position(|byte| *byte == b';')?;
    let payload = &body[separator + 1..];
    if payload.is_empty() || payload == b"?" {
        return None;
    }
    let decoded = decode_pane_clipboard_write_payload(payload)?;
    // A decoded length of 0 is possible when the base64 symbols round to zero
    // output bytes (e.g. a lone `=` sequence). tmux drops these too.
    if decoded.is_empty() {
        return None;
    }
    Some(decoded)
}

/// Same validation as `osc52_clipboard_write_payload` reduced to a boolean,
/// exposed to the outer-forward gate in `passthrough.rs`. Kept in this module
/// so both paths share the exact same decoder.
pub(super) fn osc52_payload_decodes(payload: &[u8]) -> bool {
    decode_pane_clipboard_write_payload(payload)
        .map(|decoded| !decoded.is_empty())
        .unwrap_or(false)
}

struct PaneGroundTimerJob {
    transcript: SharedPaneTranscript,
    timer: PaneGroundTimer,
}

fn schedule_pane_ground_timer(
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    timer: PaneGroundTimer,
) {
    let job = PaneGroundTimerJob { transcript, timer };
    if let Err(error) = pane_ground_timer_tx().send(job) {
        warn!(
            session = %session_name,
            pane_id = pane_id.as_u32(),
            "failed to schedule pane parser ground timer: {error}"
        );
    }
}

fn pane_ground_timer_tx() -> &'static std_mpsc::Sender<PaneGroundTimerJob> {
    static TIMER_TX: OnceLock<std_mpsc::Sender<PaneGroundTimerJob>> = OnceLock::new();
    TIMER_TX.get_or_init(|| {
        let (tx, rx) = std_mpsc::channel();
        spawn_pane_ground_timer_worker(rx);
        tx
    })
}

fn spawn_pane_ground_timer_worker(rx: std_mpsc::Receiver<PaneGroundTimerJob>) {
    let thread_name = "rmux-pane-ground-timer".to_owned();
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || run_pane_ground_timer_worker(rx))
    {
        warn!(
            thread = %thread_name,
            "failed to spawn pane parser ground timer worker: {error}"
        );
    }
}

fn run_pane_ground_timer_worker(rx: std_mpsc::Receiver<PaneGroundTimerJob>) {
    let mut jobs = Vec::<PaneGroundTimerJob>::new();
    loop {
        if jobs.is_empty() {
            match rx.recv() {
                Ok(job) => {
                    jobs.push(job);
                    continue;
                }
                Err(_) => return,
            }
        }

        expire_due_pane_ground_timers(&mut jobs);
        if jobs.is_empty() {
            continue;
        }

        let next_deadline = jobs
            .iter()
            .map(|job| job.timer.deadline)
            .min()
            .expect("non-empty timer job queue has a deadline");
        let timeout = next_deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(job) => jobs.push(job),
            Err(std_mpsc::RecvTimeoutError::Timeout) => expire_due_pane_ground_timers(&mut jobs),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn expire_due_pane_ground_timers(jobs: &mut Vec<PaneGroundTimerJob>) {
    let now = Instant::now();
    let mut index = 0;
    while index < jobs.len() {
        if now < jobs[index].timer.deadline {
            index += 1;
            continue;
        }
        let job = jobs.swap_remove(index);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            expire_pane_ground_timer_job(job);
        }))
        .is_err()
        {
            warn!("pane parser ground timer job panicked; timer worker is continuing");
        }
    }
}

fn expire_pane_ground_timer_job(job: PaneGroundTimerJob) {
    let mut transcript = match job.transcript.lock() {
        Ok(transcript) => transcript,
        Err(poisoned) => {
            warn!(
                "pane transcript mutex was poisoned while expiring parser ground timer; \
                 recovering timer worker"
            );
            poisoned.into_inner()
        }
    };
    let _ = transcript.expire_ground_timer(job.timer);
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
    eof_state: PaneOutputEofState,
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
                eof_state,
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::{
        osc52_clipboard_write_payload, osc52_payload_is_clipboard_set, publish_pane_bytes,
        PanePublishContext,
    };
    use rmux_core::{
        input::InputEndType, PaneId, TerminalClipboardQuery, TerminalPassthrough,
        TerminalPassthroughKind,
    };
    use rmux_proto::{SessionName, TerminalSize};

    use crate::pane_io::{pane_output_channel, PaneAlertCallback};
    use crate::pane_transcript::PaneTranscript;

    #[test]
    fn osc52_clipboard_set_requires_write_payload() {
        assert!(osc52_payload_is_clipboard_set(b"\x1b]52;c;aGk=\x07"));
        assert!(osc52_payload_is_clipboard_set(b"\x1b]52;c;aGk\x1b\\"));
        assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;?\x07"));
        assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;%%\x07"));
        assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;abcde\x07"));
        // The classifier must reject the same payloads the write decoder
        // rejects, otherwise the PaneSetClipboard hook fires without a stored
        // buffer (diverges from the frozen tmux 3.7b oracle: paste_add and
        // notify_pane must fire together or not at all).
        assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;\x07"));
        assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;==\x07"));
        assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;!!!\x07"));
    }

    #[test]
    fn decode_base64_standard_handles_padding_and_missing_padding() {
        assert_eq!(
            crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"aGVsbG8=").as_deref(),
            Some(&b"hello"[..])
        );
        // OSC 52 producers sometimes omit the trailing padding.
        assert_eq!(
            crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"aGVsbG8").as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(
            crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"aGk=").as_deref(),
            Some(&b"hi"[..])
        );
        assert_eq!(
            crate::clipboard_protocol::decode_pane_clipboard_write_payload(b""),
            None
        );
        // Invalid symbols / lengths are rejected rather than decoded to garbage.
        assert_eq!(
            crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"%%"),
            None
        );
        assert_eq!(
            crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"abcde"),
            None
        );
    }

    #[test]
    fn osc52_clipboard_write_payload_decodes_writes_and_skips_queries() {
        assert_eq!(
            osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
                b"\x1b]52;c;aGVsbG8=\x07".to_vec()
            ))
            .as_deref(),
            Some(&b"hello"[..])
        );
        // The ST terminator form decodes identically.
        assert_eq!(
            osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
                b"\x1b]52;c;aGk\x1b\\".to_vec()
            ))
            .as_deref(),
            Some(&b"hi"[..])
        );
        // A query carries no write payload.
        assert!(
            osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
                b"\x1b]52;c;?\x07".to_vec()
            ))
            .is_none()
        );
        // A non-clipboard passthrough is ignored.
        assert!(osc52_clipboard_write_payload(&TerminalPassthrough::raw(
            0,
            0,
            b"\x1b]52;c;aGk=\x07".to_vec()
        ))
        .is_none());
    }

    #[test]
    fn osc52_empty_payload_is_dropped_matching_oracle() {
        // tmux 3.7b's input_osc_52 returns early on an empty payload — no
        // paste_add, no outer forward. rmux must not create an empty buffer.
        assert!(
            osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
                b"\x1b]52;c;\x07".to_vec()
            ))
            .is_none()
        );
        // Neither should a base64 payload that decodes to zero bytes (a lone
        // padding sequence).
        assert!(
            osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
                b"\x1b]52;c;==\x07".to_vec()
            ))
            .is_none()
        );
    }

    #[test]
    fn osc52_invalid_base64_payload_is_dropped_matching_oracle() {
        assert!(
            osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
                b"\x1b]52;c;!!!\x07".to_vec()
            ))
            .is_none()
        );
        assert!(
            osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
                b"\x1b]52;c;@@@@\x07".to_vec()
            ))
            .is_none()
        );
    }

    #[test]
    fn title_alert_callback_runs_inside_the_transcript_publication_boundary() {
        let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
        let callback_transcript = Arc::clone(&transcript);
        let callback_observed = Arc::new(AtomicBool::new(false));
        let callback_observed_clone = Arc::clone(&callback_observed);
        let callback: PaneAlertCallback = Arc::new(move |event| {
            assert!(event.title_change.is_some(), "OSC 2 must change the title");
            assert!(
                callback_transcript.try_lock().is_err(),
                "the title callback must run before the transcript publication lock is released"
            );
            callback_observed_clone.store(true, Ordering::Release);
        });
        let output = pane_output_channel();
        let session_name = SessionName::new("title-linearization").expect("valid session name");

        let _ = publish_pane_bytes(
            PanePublishContext {
                session_name: &session_name,
                pane_id: PaneId::new(1),
                transcript: &transcript,
                pane_output: &output,
                generation: None,
                pane_alert_callback: Some(&callback),
                emit_no_bell_alert: false,
            },
            b"\x1b]2;linearized-title\x07".to_vec(),
        );

        assert!(callback_observed.load(Ordering::Acquire));
    }

    #[test]
    fn mouse_mode_alert_is_stamped_by_production_pane_publication() {
        let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
        let callback_transcript = Arc::clone(&transcript);
        let callback_observed = Arc::new(AtomicBool::new(false));
        let callback_observed_clone = Arc::clone(&callback_observed);
        let callback: PaneAlertCallback = Arc::new(move |event| {
            assert!(
                event.mouse_mode_changed,
                "the reader publication path must stamp a real mouse-mode transition"
            );
            assert!(
                callback_transcript.try_lock().is_err(),
                "mouse-mode comparison and callback must share the transcript boundary"
            );
            callback_observed_clone.store(true, Ordering::Release);
        });
        let output = pane_output_channel();
        let session_name = SessionName::new("mouse-mode-stamping").expect("valid session name");

        let _ = publish_pane_bytes(
            PanePublishContext {
                session_name: &session_name,
                pane_id: PaneId::new(1),
                transcript: &transcript,
                pane_output: &output,
                generation: None,
                pane_alert_callback: Some(&callback),
                emit_no_bell_alert: false,
            },
            b"\x1b[?1003h".to_vec(),
        );

        assert!(callback_observed.load(Ordering::Acquire));
    }

    #[test]
    fn clipboard_query_is_typed_by_production_pane_publication() {
        let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
        let callback_observed = Arc::new(AtomicBool::new(false));
        let callback_observed_clone = Arc::clone(&callback_observed);
        let callback: PaneAlertCallback = Arc::new(move |event| {
            assert!(
                !event.clipboard_set,
                "a query must not be classified as a write"
            );
            assert!(event.clipboard_writes.is_empty());
            assert_eq!(
                event.clipboard_queries,
                vec![TerminalClipboardQuery::new("zzpc", InputEndType::St)]
            );
            callback_observed_clone.store(true, Ordering::Release);
        });
        let output = pane_output_channel();
        let mut output_rx = output.subscribe();
        let session_name =
            SessionName::new("clipboard-query-stamping").expect("valid session name");

        let _ = publish_pane_bytes(
            PanePublishContext {
                session_name: &session_name,
                pane_id: PaneId::new(1),
                transcript: &transcript,
                pane_output: &output,
                generation: None,
                pane_alert_callback: Some(&callback),
                emit_no_bell_alert: false,
            },
            b"\x1b]52;zzpc;?\x1b\\".to_vec(),
        );

        assert!(callback_observed.load(Ordering::Acquire));
        let frame = output_rx
            .try_recv()
            .expect("published pane frame remains observable");
        let rmux_core::events::OutputCursorItem::Event(frame) = frame else {
            panic!("published pane frame must be an event");
        };
        assert_eq!(frame.passthroughs().len(), 1);
        assert_eq!(
            frame.passthroughs()[0].kind(),
            TerminalPassthroughKind::Clipboard
        );
        assert!(
            frame.passthroughs()[0].render_sequence().is_empty(),
            "the generic passthrough renderer must never leak a query outward"
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

        let contents =
            wait_for_file_contents(&output, b"\x1b[?1;2c".len(), Duration::from_secs(30)).await?;
        let _ = spawned.child_mut().wait();
        output_reader_task.abort();
        let _ = fs::remove_file(&output);

        assert_eq!(contents, b"\x1b[?1;2c");
        Ok(())
    }

    #[tokio::test]
    async fn async_output_reader_uses_server_runtime_when_spawned_from_temporary_runtime(
    ) -> Result<(), Box<dyn Error>> {
        // Gate marker production on PTY input without starting an interactive
        // prompt: echoed input can otherwise interleave with shell startup.
        let spawned = ChildCommand::new("sh")
            .args([
                "-c",
                "read _; printf '%s\\n' RMUX_SERVER_RUNTIME_OK; read _",
            ])
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

        writer.write_all(b"start\n")?;
        let captured = wait_for_transcript(
            &transcript_for_assertion,
            "RMUX_SERVER_RUNTIME_OK",
            Duration::from_secs(4),
        )
        .await;

        spawned.child().terminate_forcefully()?;
        output_reader_task.abort();
        drop(writer);
        let (master, mut child) = spawned.into_parts();
        drop(master);

        let reap_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if child.try_wait()?.is_some() {
                break;
            }
            if Instant::now() >= reap_deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "temporary-runtime PTY child did not exit after forceful termination",
                )
                .into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

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
        minimum_len: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            match fs::read(path) {
                Ok(contents) if contents.len() >= minimum_len => return Ok(contents),
                Ok(_) | Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Ok(contents) => {
                    return Err(format!(
                        "timed out waiting for {} to contain at least {minimum_len} bytes; got {}",
                        path.display(),
                        contents.len()
                    )
                    .into());
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
mod windows_tests {
    use std::error::Error;
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
    use rmux_proto::{SessionName, TerminalSize};
    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::{
        expire_due_pane_ground_timers, spawn_pane_output_reader, PaneGroundTimerJob,
        PaneOutputEofState,
    };
    use crate::pane_io::pane_output_channel;
    use crate::pane_transcript::PaneTranscript;

    #[test]
    fn pane_ground_timer_worker_survives_poisoned_transcript_mutex() {
        let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
        let mut timer = transcript
            .lock()
            .expect("new transcript mutex is healthy")
            .append_bytes_with_effects(b"\x1bPunterminated")
            .ground_timer
            .expect("unterminated DCS arms parser ground timer");
        timer.deadline = Instant::now() - Duration::from_millis(1);

        let poisoned = transcript.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.lock().expect("mutex is healthy before poison");
            panic!("poison pane transcript mutex for timer worker test");
        });

        let mut jobs = vec![PaneGroundTimerJob { transcript, timer }];
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            expire_due_pane_ground_timers(&mut jobs);
        }));

        assert!(result.is_ok(), "timer worker must survive poisoned jobs");
        assert!(jobs.is_empty(), "due poisoned job should be consumed");
    }

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
            PaneOutputEofState::default(),
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

    #[test]
    fn windows_output_reader_publishes_eof_exit_event_after_child_exit(
    ) -> Result<(), Box<dyn Error>> {
        let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
            .args(["/D", "/K"])
            .size(PtyTerminalSize::new(100, 30))
            .spawn()?;
        let output_reader = spawned.master().try_clone()?;
        let writer = spawned.master().try_clone_io()?;
        let transcript = PaneTranscript::shared(
            2_000,
            TerminalSize {
                cols: 100,
                rows: 30,
            },
        );
        let pane_output = pane_output_channel();
        let (tx, rx) = mpsc::channel();
        let callback: crate::pane_io::PaneExitCallback = Arc::new(move |event| {
            let _ = tx.send(event.output_eof_published());
        });

        spawn_pane_output_reader(
            SessionName::new("alpha").expect("valid session name"),
            PaneId::new(1),
            output_reader,
            transcript,
            pane_output,
            Some(7),
            PaneOutputEofState::default(),
            None,
            Some(callback),
        );

        writer.write_all(b"exit\r\n")?;
        let _ = spawned.child_mut().wait()?;
        spawned.child().close_pseudoconsole();

        let published = rx.recv_timeout(Duration::from_secs(2))?;
        assert!(
            published,
            "Windows reader must report EOF as already published"
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
