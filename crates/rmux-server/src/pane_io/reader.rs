use std::io;

use rmux_core::{PaneId, TerminalPassthrough, TerminalPassthroughKind};
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::{PtyIo, PtyMaster};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(windows, test))]
use std::sync::Arc;
use std::sync::{mpsc as std_mpsc, OnceLock};
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
use crate::pane_transcript::{PaneGroundTimer, PaneSynchronizedOutputTimer, SharedPaneTranscript};

#[cfg(unix)]
const PANE_BLOCKING_PARSE_MIN_BYTES: usize = READ_BUFFER_SIZE;
#[cfg(unix)]
const PANE_READ_BATCH_TRIGGER_BYTES: usize = 1;
#[cfg(unix)]
const PANE_READ_BATCH_LIMIT: usize = 64;
#[cfg(unix)]
const PANE_READ_BATCH_MAX_BYTES: usize = 4 * 1024 * 1024;
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

#[derive(Clone, Copy)]
struct PanePublishContext<'a> {
    session_name: &'a rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: &'a SharedPaneTranscript,
    pane_output: &'a PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<&'a PaneAlertCallback>,
    status_terminal: Option<&'a PtyIo>,
    emit_no_bell_alert: bool,
}

struct OwnedPanePublishContext {
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    status_terminal: Option<PtyIo>,
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
    let lifecycle_pane_output = pane_output.clone();
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
            lifecycle_pane_output.retire_reader_for_generation(generation);
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
    pane_output: PaneOutputSender,
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
            let process_exit = child.wait_for_process_tree_exit();
            // Closing ConPTY may need its output drained. Wake an all-OFF
            // reader before close (including forceful error teardown), and
            // let the reader publish the final bytes and retire itself at EOF.
            pane_output.drain_reader_for_generation(generation);
            if let Err(error) = process_exit {
                warn!(
                    session = %session_name,
                    pane_id = pane_id.as_u32(),
                    "failed to wait for pane process tree before closing ConPTY: {error}"
                );
                if let Err(teardown_error) = child.terminate_forcefully() {
                    warn!(
                        session = %session_name,
                        pane_id = pane_id.as_u32(),
                        "failed to terminate pane process tree after liveness query failure: \
                         {teardown_error}"
                    );
                    child.close_pseudoconsole();
                }
            } else {
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
        if !pane_output
            .wait_for_reader_demand(pane_id.as_u32(), generation)
            .await
        {
            return Ok(());
        }
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
            publish_pane_eof(&transcript, &pane_output, generation);
            pane_output.retire_reader_for_generation(generation);
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

        let mut bytes = Vec::with_capacity(bytes_read.saturating_mul(4));
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
        // Policy validation and parsing share one transcript guard. Capacity
        // waits happen only after a pre-effect refusal, outside both locks.
        let (replies, admission) = publish_pane_bytes_admitted(
            PanePublishContext {
                session_name: &session_name,
                pane_id,
                transcript: &transcript,
                pane_output: &pane_output,
                generation,
                pane_alert_callback: pane_alert_callback.as_ref(),
                status_terminal: Some(pane_reader.get_ref()),
                emit_no_bell_alert,
            },
            bytes,
            published_bytes >= PANE_BLOCKING_PARSE_MIN_BYTES || sustained_small_reads,
        )
        .await?;
        drop(admission);
        write_parser_replies_to_pane(&pane_reader, replies).await?;
        if !pane_output.wait_for_control_admission(generation).await {
            return Ok(());
        }
        read_bytes_since_heap_trim = read_bytes_since_heap_trim.saturating_add(published_bytes);
        if read_bytes_since_heap_trim >= PANE_READ_BYTES_BEFORE_HEAP_TRIM {
            read_bytes_since_heap_trim = 0;
            maybe_trim_process_heap_after(PANE_READ_BYTES_BEFORE_HEAP_TRIM);
        }
        // Ring consumers must copy this publication before another ready PTY
        // batch can rotate it away. A short, unsaturated burst can fill the
        // ring without reaching either of the reader's existing yield limits.
        if read_saturated || sustained_small_reads || pane_output.has_ring_receivers() {
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
        if !pane_output.wait_for_reader_demand_blocking(pane_id.as_u32(), generation) {
            return Ok(());
        }
        let bytes_read = match pane_reader.read(&mut buffer) {
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if bytes_read == 0 {
            publish_pane_eof(&transcript, &pane_output, generation);
            pane_output.retire_reader_for_generation(generation);
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

        let (replies, admission) = publish_pane_bytes_admitted_blocking(
            PanePublishContext {
                session_name: &session_name,
                pane_id,
                transcript: &transcript,
                pane_output: &pane_output,
                generation,
                pane_alert_callback: pane_alert_callback.as_ref(),
                status_terminal: None,
                emit_no_bell_alert: true,
            },
            buffer[..bytes_read].to_vec(),
        );
        drop(admission);
        write_parser_replies_to_pane_blocking(&pane_reader, replies)?;
    }
}

#[cfg(test)]
mod transcript_policy_tests;

#[cfg(test)]
#[path = "reader/program_status_tests.rs"]
mod program_status_tests;

#[cfg(test)]
mod cursor_default_tests;

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod unix_tests;

#[cfg(all(test, windows))]
mod windows_tests;

#[cfg(test)]
mod rqss_tests;

#[cfg(test)]
mod rqm_tests;

#[cfg(unix)]
mod heap_trim;
#[cfg(unix)]
use heap_trim::{maybe_trim_process_heap_after, PANE_READ_BYTES_BEFORE_HEAP_TRIM};

#[cfg(all(test, unix))]
mod rename_admission_tests;

mod publication;
mod timers;
pub(super) use publication::osc52_payload_decodes;
use publication::*;
#[cfg(test)]
pub(crate) use publication::{
    publish_pane_bytes_capturing_alerts, publish_pane_bytes_for_test,
    publish_pane_bytes_through_effects,
};
use timers::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::spawn_blocking_pane_output_reader_inner;

#[cfg(all(test, any(unix, windows)))]
mod rename_admission_native_tests;

#[cfg(all(test, unix))]
pub(crate) use publication::publish_pane_eof_for_test;
#[cfg(all(test, windows))]
mod windows_reply_probe;
