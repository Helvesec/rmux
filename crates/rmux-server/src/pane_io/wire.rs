use std::future::pending;
use std::io;
#[cfg(unix)]
use std::time::Duration;

use rmux_core::events::OutputCursorItem;
use rmux_ipc::{is_peer_disconnect, LocalStream};
use rmux_proto::{encode_attach_message, AttachFrameDecoder, AttachMessage};
#[cfg(unix)]
use rmux_pty::PtyIo;
#[cfg(unix)]
use rmux_pty::PtyMaster;
#[cfg(unix)]
use tokio::io::unix::AsyncFd;
use tracing::warn;

use crate::outer_terminal::OuterTerminal;

use super::types::{AttachTarget, OpenAttachTarget, PaneOutputReceiver};

#[cfg(unix)]
// Eight 8 KiB immediate reads lets a pane deliver roughly 64 KiB of queued
// output before yielding, which keeps startup snappy without monopolizing a
// Tokio worker when a pane is producing sustained output.
const MAX_IMMEDIATE_PANE_READS: usize = 8;
#[cfg(unix)]
const MAX_STARTUP_EIO_READS: usize = 256;
#[cfg(unix)]
const STARTUP_EIO_YIELD_READS: usize = 8;
#[cfg(unix)]
const STARTUP_EIO_BACKOFF: Duration = Duration::from_millis(1);

#[cfg(unix)]
#[derive(Debug, Default)]
pub(super) struct PaneReadinessState {
    immediate_reads: usize,
    startup_eio_reads: usize,
    established: bool,
    startup_eio_exhausted: bool,
}

pub(super) fn open_attach_target(target: AttachTarget) -> io::Result<OpenAttachTarget> {
    let AttachTarget {
        session_name,
        pane_master,
        pane_output,
        render_frame,
        outer_terminal,
        cursor_style,
        active_pane_geometry,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id,
        live_pane,
        active_pane_id,
    } = target;
    Ok(OpenAttachTarget {
        session_name,
        pane_master,
        pane_output: Some(pane_output.subscribe()),
        render_frame,
        outer_terminal,
        cursor_style,
        active_pane_geometry,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id,
        live_pane,
        active_pane_id,
    })
}

#[cfg(unix)]
pub(super) fn open_pane_writer(pane_master: PtyMaster) -> io::Result<(AsyncFd<PtyIo>, PtyIo)> {
    let pane_writer = pane_master.into_io();
    let reply_writer = pane_writer.try_clone().map_err(io::Error::other)?;
    // PtyIo::try_clone() uses dup(2), so both handles share one open file
    // description. O_NONBLOCK applies to the reply writer too; Unix PTY writes
    // go through unix_io::write_all(), which handles EAGAIN with poll().
    pane_writer.set_nonblocking()?;

    Ok((AsyncFd::new(pane_writer)?, reply_writer))
}

pub(super) async fn emit_render_frame(
    stream: &LocalStream,
    outer_terminal: &OuterTerminal,
    render_frame: &[u8],
) -> io::Result<()> {
    let frame = outer_terminal.wrap_render_frame(render_frame);
    emit_attach_bytes(stream, &frame).await
}

pub(super) async fn read_socket_bytes(
    stream: &LocalStream,
    decoder: &mut AttachFrameDecoder,
    buffer: &mut [u8],
) -> io::Result<bool> {
    loop {
        stream.readable().await?;
        match stream.try_read(buffer) {
            Ok(0) => return Ok(false),
            Ok(bytes_read) => {
                decoder.push_bytes(&buffer[..bytes_read]);
                return Ok(true);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        }
    }
}

pub(super) enum TrySocketRead {
    Read,
    Closed,
    WouldBlock,
}

pub(super) fn try_read_socket_bytes(
    stream: &LocalStream,
    decoder: &mut AttachFrameDecoder,
    buffer: &mut [u8],
) -> io::Result<TrySocketRead> {
    match stream.try_read(buffer) {
        Ok(0) => Ok(TrySocketRead::Closed),
        Ok(bytes_read) => {
            decoder.push_bytes(&buffer[..bytes_read]);
            Ok(TrySocketRead::Read)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(TrySocketRead::WouldBlock),
        Err(error) => Err(error),
    }
}

pub(super) async fn emit_attach_message(
    stream: &LocalStream,
    message: &AttachMessage,
) -> io::Result<()> {
    let frame = encode_attach_message(message).map_err(io::Error::other)?;
    emit_attach_bytes(stream, &frame).await
}

pub(super) async fn emit_attach_frame(
    stream: &LocalStream,
    message: &AttachMessage,
) -> io::Result<()> {
    let frame = encode_attach_message(message).map_err(io::Error::other)?;
    write_all_to_stream(stream, &frame).await
}

pub(super) async fn recv_pane_output(
    pane_output: &mut PaneOutputReceiver,
) -> io::Result<OutputCursorItem> {
    match pane_output.recv().await {
        OutputCursorItem::Event(event) => Ok(OutputCursorItem::Event(event)),
        OutputCursorItem::Gap(gap) => {
            warn!(
                expected_sequence = gap.expected_sequence(),
                resume_sequence = gap.resume_sequence(),
                missed_events = gap.missed_events(),
                recent_bytes = gap.recent_snapshot().len(),
                "attach pane output receiver lagged"
            );
            Ok(OutputCursorItem::Gap(gap))
        }
    }
}

pub(super) async fn recv_pane_output_optional(
    pane_output: Option<&mut PaneOutputReceiver>,
) -> io::Result<Option<OutputCursorItem>> {
    match pane_output {
        Some(pane_output) => recv_pane_output(pane_output).await.map(Some),
        None => pending().await,
    }
}

pub(super) async fn emit_attach_data_frame(stream: &LocalStream, bytes: &[u8]) -> io::Result<()> {
    let frame =
        encode_attach_message(&AttachMessage::Data(bytes.to_vec())).map_err(io::Error::other)?;
    write_all_to_stream(stream, &frame).await
}

pub(super) async fn emit_attach_bytes(stream: &LocalStream, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }

    emit_attach_data_frame(stream, bytes).await
}

pub(super) async fn emit_attach_stop(
    stream: &LocalStream,
    current_target: &OpenAttachTarget,
) -> io::Result<()> {
    emit_attach_bytes(
        stream,
        &current_target.outer_terminal.attach_stop_sequence(),
    )
    .await
}

pub(super) async fn emit_detached_message(
    stream: &LocalStream,
    current_target: &OpenAttachTarget,
) -> io::Result<()> {
    emit_attach_bytes(
        stream,
        format!(
            "[detached (from session {})]\r\n",
            current_target.session_name
        )
        .as_bytes(),
    )
    .await
}

pub(super) async fn emit_exited_message(stream: &LocalStream) -> io::Result<()> {
    emit_attach_bytes(stream, b"[exited]\r\n").await
}

#[cfg(unix)]
pub(super) async fn read_from_pane(
    pane_reader: &AsyncFd<PtyIo>,
    readiness: &mut PaneReadinessState,
    buffer: &mut [u8],
) -> io::Result<usize> {
    loop {
        if readiness.immediate_reads >= MAX_IMMEDIATE_PANE_READS {
            readiness.immediate_reads = 0;
            tokio::task::yield_now().await;
        }

        // Read once before awaiting readiness. A pane can emit its initial
        // prompt/output before the async task reaches readable().await; this
        // preserves AsyncFd while avoiding dependence on a later readiness edge.
        match try_read_pane_now(pane_reader.get_ref(), buffer)? {
            PaneRead::Bytes(bytes_read) => {
                readiness.record_immediate_bytes(bytes_read);
                return Ok(bytes_read);
            }
            PaneRead::NotReady => {}
            PaneRead::SlaveUnavailable => match readiness.retry_startup_eio() {
                StartupEioReadiness::Retry(delay) => {
                    delay_startup_eio_retry(delay).await;
                    continue;
                }
                StartupEioReadiness::StartupRetriesExhausted
                | StartupEioReadiness::EstablishedEof => return Ok(0),
            },
        }
        readiness.immediate_reads = 0;

        let mut ready = pane_reader.readable().await?;
        match ready.try_io(|inner| inner.get_ref().read(&mut *buffer)) {
            Ok(Ok(bytes_read)) => {
                readiness.record_ready_bytes(bytes_read);
                return Ok(bytes_read);
            }
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error))
                if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
            {
                match readiness.retry_startup_eio() {
                    StartupEioReadiness::Retry(delay) => {
                        delay_startup_eio_retry(delay).await;
                        continue;
                    }
                    StartupEioReadiness::StartupRetriesExhausted
                    | StartupEioReadiness::EstablishedEof => return Ok(0),
                }
            }
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => continue,
        }
    }
}

#[cfg(unix)]
async fn delay_startup_eio_retry(delay: StartupEioRetryDelay) {
    match delay {
        StartupEioRetryDelay::Yield => tokio::task::yield_now().await,
        StartupEioRetryDelay::Sleep(duration) => tokio::time::sleep(duration).await,
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupEioReadiness {
    Retry(StartupEioRetryDelay),
    StartupRetriesExhausted,
    EstablishedEof,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupEioRetryDelay {
    Yield,
    Sleep(Duration),
}

#[cfg(unix)]
impl StartupEioRetryDelay {
    fn for_attempt(attempt: usize) -> Self {
        if attempt <= STARTUP_EIO_YIELD_READS {
            Self::Yield
        } else {
            Self::Sleep(STARTUP_EIO_BACKOFF)
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod startup_eio_retry_delay_tests {
    use super::{
        PaneReadinessState, StartupEioReadiness, StartupEioRetryDelay, STARTUP_EIO_BACKOFF,
        STARTUP_EIO_YIELD_READS,
    };

    #[test]
    fn startup_eio_retries_yield_before_backing_off() {
        for attempt in 1..=STARTUP_EIO_YIELD_READS {
            assert_eq!(
                StartupEioRetryDelay::for_attempt(attempt),
                StartupEioRetryDelay::Yield
            );
        }
        assert_eq!(
            StartupEioRetryDelay::for_attempt(STARTUP_EIO_YIELD_READS + 1),
            StartupEioRetryDelay::Sleep(STARTUP_EIO_BACKOFF)
        );
    }

    #[test]
    fn readiness_state_tracks_startup_eio_until_output_is_established() {
        let mut readiness = PaneReadinessState::default();

        assert_eq!(
            readiness.retry_startup_eio(),
            StartupEioReadiness::Retry(StartupEioRetryDelay::Yield)
        );
        assert_eq!(readiness.startup_eio_reads, 1);

        readiness.record_ready_bytes(1);

        assert!(readiness.established);
        assert_eq!(readiness.startup_eio_reads, 0);
        assert_eq!(
            readiness.retry_startup_eio(),
            StartupEioReadiness::EstablishedEof
        );
        assert!(!readiness.startup_eio_exhausted());
    }

    #[test]
    fn readiness_state_caps_startup_eio_retries() {
        let mut readiness = PaneReadinessState::default();

        for _ in 0..super::MAX_STARTUP_EIO_READS {
            assert!(matches!(
                readiness.retry_startup_eio(),
                StartupEioReadiness::Retry(_)
            ));
        }

        assert_eq!(
            readiness.retry_startup_eio(),
            StartupEioReadiness::StartupRetriesExhausted
        );
        assert!(readiness.startup_eio_exhausted());
    }
}

#[cfg(unix)]
impl PaneReadinessState {
    fn record_immediate_bytes(&mut self, bytes_read: usize) {
        self.startup_eio_reads = 0;
        self.startup_eio_exhausted = false;
        if bytes_read > 0 {
            self.immediate_reads += 1;
            self.established = true;
        } else {
            self.immediate_reads = 0;
        }
    }

    fn record_ready_bytes(&mut self, bytes_read: usize) {
        self.immediate_reads = 0;
        self.startup_eio_reads = 0;
        self.startup_eio_exhausted = false;
        if bytes_read > 0 {
            self.established = true;
        }
    }

    pub(super) fn startup_eio_exhausted(&self) -> bool {
        self.startup_eio_exhausted
    }

    pub(super) fn startup_eio_reads(&self) -> usize {
        self.startup_eio_reads
    }

    fn retry_startup_eio(&mut self) -> StartupEioReadiness {
        self.immediate_reads = 0;
        // Unix PTY masters can report EIO as EOF. Linux can also report it
        // briefly before the slave side has reached a stable post-spawn state.
        // Before the first successful read, treat a bounded run of EIO as
        // startup latency; after output is established, EIO is normal EOF.
        if self.established {
            return StartupEioReadiness::EstablishedEof;
        }
        if self.startup_eio_reads >= MAX_STARTUP_EIO_READS {
            self.startup_eio_exhausted = true;
            return StartupEioReadiness::StartupRetriesExhausted;
        }
        self.startup_eio_reads += 1;
        StartupEioReadiness::Retry(StartupEioRetryDelay::for_attempt(self.startup_eio_reads))
    }
}

#[cfg(unix)]
enum PaneRead {
    Bytes(usize),
    NotReady,
    SlaveUnavailable,
}

#[cfg(unix)]
fn try_read_pane_now(reader: &PtyIo, buffer: &mut [u8]) -> io::Result<PaneRead> {
    match reader.read(buffer) {
        Ok(bytes_read) => Ok(PaneRead::Bytes(bytes_read)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(PaneRead::NotReady),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(PaneRead::NotReady),
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
            Ok(PaneRead::SlaveUnavailable)
        }
        Err(error) => Err(error),
    }
}

async fn write_all_to_stream(stream: &LocalStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.writable().await?;

        match stream.try_write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write returned 0 while forwarding pane bytes",
                ));
            }
            Ok(bytes_written) => bytes = &bytes[bytes_written..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if is_peer_disconnect(&error) => return Ok(()),
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

pub(super) fn invalid_attach_message(error: rmux_proto::RmuxError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::error::Error;
    use std::io;
    use std::time::{Duration, Instant};

    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::{open_pane_writer, read_from_pane, PaneReadinessState};

    #[tokio::test]
    async fn read_from_pane_consumes_output_written_before_readiness_wait(
    ) -> Result<(), Box<dyn Error>> {
        let mut spawned = ChildCommand::new("sh")
            .args(["-c", "printf PRE_READY; sleep 1"])
            .size(PtyTerminalSize::new(80, 24))
            .spawn()?;
        let output_reader = spawned.master().try_clone()?;

        tokio::time::sleep(Duration::from_millis(100)).await;
        let (pane_reader, _reply_writer) = open_pane_writer(output_reader)?;
        let captured =
            read_until_contains(&pane_reader, "PRE_READY", Duration::from_millis(500)).await?;

        spawned.child().terminate_forcefully()?;
        let _ = spawned.child_mut().wait()?;

        assert!(
            captured.contains("PRE_READY"),
            "expected pre-existing pane output, got {captured:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn read_from_pane_consumes_output_written_after_registration_before_wait(
    ) -> Result<(), Box<dyn Error>> {
        let mut spawned = ChildCommand::new("sh")
            .args(["-c", "sleep 0.05; printf POST_REGISTER; sleep 1"])
            .size(PtyTerminalSize::new(80, 24))
            .spawn()?;
        let output_reader = spawned.master().try_clone()?;
        let (pane_reader, _reply_writer) = open_pane_writer(output_reader)?;

        tokio::time::sleep(Duration::from_millis(100)).await;
        let captured =
            read_until_contains(&pane_reader, "POST_REGISTER", Duration::from_millis(500)).await?;

        spawned.child().terminate_forcefully()?;
        let _ = spawned.child_mut().wait()?;

        assert!(
            captured.contains("POST_REGISTER"),
            "expected post-registration pane output, got {captured:?}"
        );
        Ok(())
    }

    async fn read_until_contains(
        pane_reader: &tokio::io::unix::AsyncFd<rmux_pty::PtyIo>,
        needle: &str,
        timeout: Duration,
    ) -> io::Result<String> {
        let deadline = Instant::now() + timeout;
        let mut readiness = PaneReadinessState::default();
        let mut buffer = [0_u8; 256];
        let mut captured = Vec::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let bytes_read = tokio::time::timeout(
                remaining,
                read_from_pane(pane_reader, &mut readiness, &mut buffer),
            )
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))??;
            if bytes_read == 0 {
                break;
            }
            captured.extend_from_slice(&buffer[..bytes_read]);
            let rendered = String::from_utf8_lossy(&captured);
            if rendered.contains(needle) {
                return Ok(rendered.into_owned());
            }
        }

        Ok(String::from_utf8_lossy(&captured).into_owned())
    }
}
