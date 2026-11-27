use std::future::pending;
use std::io;
#[cfg(unix)]
use std::time::Duration;

use rmux_core::events::{OutputCursorItem, OutputGap};
use rmux_proto::{
    encode_attach_data, encode_attach_data_into_slice, encode_attach_message, AttachFrameDecoder,
    AttachMessage, ATTACH_DATA_HEADER_LEN, DEFAULT_MAX_FRAME_LENGTH,
};
#[cfg(unix)]
use rmux_pty::PtyIo;
#[cfg(unix)]
use rmux_pty::PtyMaster;
#[cfg(unix)]
use tokio::io::unix::AsyncFd;
use tracing::warn;

use crate::outer_terminal::OuterTerminal;

use super::attach_transport::{AttachTransport, TryAttachRead};
use super::types::{AttachTarget, DrainedPaneOutput, OpenAttachTarget, PaneOutputReceiver};

#[cfg(unix)]
// Eight immediate 64 KiB reads amortize readiness wakeups under sustained PTY
// output while still yielding regularly enough to keep other panes responsive.
const MAX_IMMEDIATE_PANE_READS: usize = 8;
#[cfg(unix)]
const MAX_STARTUP_EIO_READS: usize = 256;
#[cfg(unix)]
const STARTUP_EIO_YIELD_READS: usize = 8;
#[cfg(unix)]
const STARTUP_EIO_BACKOFF: Duration = Duration::from_millis(1);
const STACK_ATTACH_DATA_PAYLOAD: usize = 1024;

#[cfg(unix)]
#[derive(Debug, Default)]
pub(super) struct PaneReadinessState {
    immediate_reads: usize,
    startup_eio_reads: usize,
    established: bool,
    startup_eio_exhausted: bool,
}

pub(super) fn open_attach_target(
    target: AttachTarget,
    render_stream: bool,
) -> io::Result<OpenAttachTarget> {
    let AttachTarget {
        session_name,
        pane_master: _,
        pane_output,
        pane_output_start_sequence: _,
        render_frame,
        outer_terminal,
        client_title: _,
        cursor_style,
        active_pane_geometry,
        raw_passthrough,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id,
        live_pane,
    } = target;
    Ok(OpenAttachTarget {
        session_name,
        predicted_echo: Default::default(),
        predicted_echo_started_at: None,
        pane_output: Some(pane_output),
        render_frame,
        outer_terminal,
        cursor_style,
        active_pane_geometry,
        raw_passthrough,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id,
        live_pane,
        render_stream,
    })
}

#[cfg(unix)]
// tokio 1.53.3 deprecates the safe AsyncFd constructors (RUSTSEC-2026-0386); this crate forbids
// unsafe, and the AsyncFd owns the PtyIo, whose fd stays open.
#[allow(deprecated)]
pub(super) fn open_pane_writer(pane_master: PtyMaster) -> io::Result<AsyncFd<PtyIo>> {
    let pane_writer = pane_master.into_io();
    AsyncFd::new(pane_writer)
}

pub(super) async fn emit_render_frame(
    stream: &AttachTransport,
    outer_terminal: &OuterTerminal,
    render_frame: &[u8],
) -> io::Result<()> {
    let Some(frame) = bounded_wrapped_render_frame(outer_terminal, render_frame) else {
        return emit_attach_bytes(stream, render_frame).await;
    };
    emit_attach_bytes(stream, &frame).await
}

pub(super) async fn emit_coalescible_render_frame(
    stream: &AttachTransport,
    outer_terminal: &OuterTerminal,
    render_frame: &[u8],
    render_stream: bool,
) -> io::Result<()> {
    if render_frame.is_empty() {
        return Ok(());
    }
    let Some(frame) = bounded_wrapped_render_frame(outer_terminal, render_frame) else {
        // Render messages are replaceable, so fragmenting one would let the
        // client discard an intermediate part. Keep a still-bounded repaint
        // replaceable without synchronized-output wrapping; larger repaints
        // use strict Data messages whose order the client must preserve.
        return if render_stream && render_frame.len() <= DEFAULT_MAX_FRAME_LENGTH {
            emit_attach_frame(stream, &AttachMessage::Render(render_frame.to_vec())).await
        } else {
            emit_attach_bytes(stream, render_frame).await
        };
    };
    if render_stream {
        emit_attach_frame(stream, &AttachMessage::Render(frame)).await
    } else {
        emit_attach_bytes(stream, &frame).await
    }
}

fn bounded_wrapped_render_frame(
    outer_terminal: &OuterTerminal,
    render_frame: &[u8],
) -> Option<Vec<u8>> {
    if render_frame.len() > DEFAULT_MAX_FRAME_LENGTH {
        return None;
    }
    let frame = outer_terminal.wrap_render_frame(render_frame);
    (frame.len() <= DEFAULT_MAX_FRAME_LENGTH).then_some(frame)
}

pub(super) async fn read_socket_bytes(
    stream: &AttachTransport,
    decoder: &mut AttachFrameDecoder,
) -> io::Result<bool> {
    stream.read_into(decoder).await
}

pub(super) fn try_read_socket_bytes(
    stream: &AttachTransport,
    decoder: &mut AttachFrameDecoder,
) -> io::Result<TryAttachRead> {
    stream.try_read_into(decoder)
}

pub(super) async fn emit_attach_message(
    stream: &AttachTransport,
    message: &AttachMessage,
) -> io::Result<()> {
    let frame = encode_attach_message(message).map_err(io::Error::other)?;
    write_all_to_stream(stream, &frame).await
}

pub(super) async fn emit_attach_frame(
    stream: &AttachTransport,
    message: &AttachMessage,
) -> io::Result<()> {
    let frame = encode_attach_message(message).map_err(io::Error::other)?;
    write_all_to_stream(stream, &frame).await
}

#[cfg(test)]
pub(super) async fn recv_pane_output(
    pane_output: &mut PaneOutputReceiver,
) -> io::Result<OutputCursorItem> {
    match pane_output.recv().await {
        OutputCursorItem::Event(event) => Ok(OutputCursorItem::Event(event)),
        OutputCursorItem::Gap(gap) => {
            warn_pane_output_gap(&gap);
            Ok(OutputCursorItem::Gap(gap))
        }
    }
}

pub(super) fn warn_pane_output_gap(gap: &OutputGap) {
    warn!(
        expected_sequence = gap.expected_sequence(),
        resume_sequence = gap.resume_sequence(),
        missed_events = gap.missed_events(),
        recent_bytes = gap.recent_snapshot().len(),
        "attach pane output receiver lagged"
    );
}

pub(super) async fn recv_pane_output_optional(
    pane_output: Option<&mut PaneOutputReceiver>,
) -> io::Result<Option<DrainedPaneOutput>> {
    match pane_output {
        Some(pane_output) => {
            let drained = pane_output.recv_drained().await;
            if let OutputCursorItem::Gap(gap) = &drained.item {
                warn_pane_output_gap(gap);
            }
            Ok(Some(drained))
        }
        None => pending().await,
    }
}

pub(super) async fn emit_attach_data_frame(
    stream: &AttachTransport,
    bytes: &[u8],
) -> io::Result<()> {
    if bytes.len() <= STACK_ATTACH_DATA_PAYLOAD {
        let mut frame = [0_u8; STACK_ATTACH_DATA_PAYLOAD + ATTACH_DATA_HEADER_LEN];
        let len = encode_attach_data_into_slice(bytes, &mut frame).map_err(io::Error::other)?;
        return write_all_to_stream(stream, &frame[..len]).await;
    }

    let frame = encode_attach_data(bytes).map_err(io::Error::other)?;
    write_all_to_stream(stream, &frame).await
}

pub(super) async fn emit_attach_bytes(stream: &AttachTransport, bytes: &[u8]) -> io::Result<()> {
    // Data frames are strict stream fragments on both attach clients. The
    // fixed codec ceiling bounds each encoder allocation independently of the
    // rendered keyframe or passthrough event that supplied these bytes.
    for chunk in bytes.chunks(DEFAULT_MAX_FRAME_LENGTH) {
        emit_attach_data_frame(stream, chunk).await?;
    }
    Ok(())
}

pub(super) async fn emit_attach_stop(
    stream: &AttachTransport,
    current_target: &OpenAttachTarget,
) -> io::Result<()> {
    emit_attach_bytes(
        stream,
        &current_target.outer_terminal.attach_stop_sequence(),
    )
    .await
}

pub(super) async fn emit_detached_attach_stop(
    stream: &AttachTransport,
    current_target: &OpenAttachTarget,
) -> io::Result<()> {
    let mut bytes = current_target.outer_terminal.attach_stop_sequence();
    bytes.extend_from_slice(
        format!(
            "[detached (from session {})]\r\n",
            current_target.session_name
        )
        .as_bytes(),
    );
    emit_attach_bytes(stream, &bytes).await
}

pub(super) async fn emit_exited_attach_stop(
    stream: &AttachTransport,
    current_target: &OpenAttachTarget,
) -> io::Result<()> {
    let mut bytes = current_target.outer_terminal.attach_stop_sequence();
    bytes.extend_from_slice(b"[exited]\r\n");
    emit_attach_bytes(stream, &bytes).await
}

/// Ends an attach stream because the server is shutting down, as tmux's
/// `MSG_SHUTDOWN` does: the stop sequence, then the `[server exited]` banner.
/// Only a client that opted into `stream.attach.server_exited` also receives
/// the typed notice that turns its exit status into a failure.
pub(super) async fn emit_server_exited_attach_stop(
    stream: &AttachTransport,
    current_target: &OpenAttachTarget,
    typed_notice: bool,
) -> io::Result<()> {
    let mut bytes = current_target.outer_terminal.attach_stop_sequence();
    bytes.extend_from_slice(b"[server exited]\r\n");
    emit_attach_bytes(stream, &bytes).await?;
    if typed_notice {
        emit_attach_message(stream, &AttachMessage::ServerExited).await?;
    }
    Ok(())
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
        let startup_read = try_read_pane_now(pane_reader.get_ref(), buffer)?;
        pane_reader.get_ref().release_startup_slave_guard();
        match startup_read {
            PaneRead::Bytes(0) if !readiness.output_established() => {
                match readiness
                    .retry_startup_eio(|| pane_reader.get_ref().session_leader_has_exited())
                {
                    StartupEioReadiness::Retry(delay) => {
                        delay_startup_eio_retry(delay).await;
                        continue;
                    }
                    StartupEioReadiness::StartupRetriesExhausted
                    | StartupEioReadiness::SessionLeaderExited
                    | StartupEioReadiness::EstablishedEof => return Ok(0),
                }
            }
            PaneRead::Bytes(bytes_read) => {
                readiness.record_immediate_bytes(bytes_read);
                return Ok(bytes_read);
            }
            PaneRead::NotReady => {}
            PaneRead::SlaveUnavailable => match readiness
                .retry_startup_eio(|| pane_reader.get_ref().session_leader_has_exited())
            {
                StartupEioReadiness::Retry(delay) => {
                    delay_startup_eio_retry(delay).await;
                    continue;
                }
                StartupEioReadiness::StartupRetriesExhausted
                | StartupEioReadiness::SessionLeaderExited
                | StartupEioReadiness::EstablishedEof => return Ok(0),
            },
        }
        readiness.immediate_reads = 0;

        let mut ready = pane_reader.readable().await?;
        match ready.try_io(|inner| inner.get_ref().try_read(&mut *buffer)) {
            Ok(Ok(0)) if !readiness.output_established() => match readiness
                .retry_startup_eio(|| pane_reader.get_ref().session_leader_has_exited())
            {
                StartupEioReadiness::Retry(delay) => {
                    delay_startup_eio_retry(delay).await;
                    continue;
                }
                StartupEioReadiness::StartupRetriesExhausted
                | StartupEioReadiness::SessionLeaderExited
                | StartupEioReadiness::EstablishedEof => return Ok(0),
            },
            Ok(Ok(bytes_read)) => {
                readiness.record_ready_bytes(bytes_read);
                return Ok(bytes_read);
            }
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error))
                if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
            {
                match readiness
                    .retry_startup_eio(|| pane_reader.get_ref().session_leader_has_exited())
                {
                    StartupEioReadiness::Retry(delay) => {
                        delay_startup_eio_retry(delay).await;
                        continue;
                    }
                    StartupEioReadiness::StartupRetriesExhausted
                    | StartupEioReadiness::SessionLeaderExited
                    | StartupEioReadiness::EstablishedEof => return Ok(0),
                }
            }
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => continue,
        }
    }
}

#[cfg(unix)]
pub(super) fn try_read_available_from_pane(
    pane_reader: &AsyncFd<PtyIo>,
    buffer: &mut [u8],
) -> io::Result<Option<usize>> {
    match try_read_pane_now(pane_reader.get_ref(), buffer)? {
        PaneRead::Bytes(bytes_read) => Ok(Some(bytes_read)),
        PaneRead::NotReady | PaneRead::SlaveUnavailable => Ok(None),
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
    /// The pane's process has exited, so the signal is its EOF, not startup.
    SessionLeaderExited,
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
            readiness.retry_startup_eio(|| false),
            StartupEioReadiness::Retry(StartupEioRetryDelay::Yield)
        );
        assert_eq!(readiness.startup_eio_reads, 1);

        readiness.record_ready_bytes(1);

        assert!(readiness.established);
        assert_eq!(readiness.startup_eio_reads, 0);
        assert_eq!(
            readiness.retry_startup_eio(|| false),
            StartupEioReadiness::EstablishedEof
        );
        assert!(!readiness.startup_eio_exhausted());
    }

    #[test]
    fn readiness_state_ends_startup_once_the_session_leader_has_exited() {
        let mut readiness = PaneReadinessState::default();

        assert_eq!(
            readiness.retry_startup_eio(|| true),
            StartupEioReadiness::SessionLeaderExited
        );
        assert_eq!(readiness.startup_eio_reads, 0);
        assert!(!readiness.startup_eio_exhausted());

        readiness.record_ready_bytes(1);
        assert_eq!(
            readiness.retry_startup_eio(|| panic!("established output needs no exit probe")),
            StartupEioReadiness::EstablishedEof
        );
    }

    #[test]
    fn readiness_state_caps_startup_eio_retries() {
        let mut readiness = PaneReadinessState::default();

        for _ in 0..super::MAX_STARTUP_EIO_READS {
            assert!(matches!(
                readiness.retry_startup_eio(|| false),
                StartupEioReadiness::Retry(_)
            ));
        }

        assert_eq!(
            readiness.retry_startup_eio(|| false),
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

    fn output_established(&self) -> bool {
        self.established
    }

    fn retry_startup_eio(
        &mut self,
        session_leader_exited: impl FnOnce() -> bool,
    ) -> StartupEioReadiness {
        self.immediate_reads = 0;
        // Unix PTY masters can report EIO or a zero-length read as EOF. Linux
        // can also report it briefly before the slave side has reached a
        // stable post-spawn state. Before the first successful read, treat a
        // bounded run of these signals as startup latency; after output is
        // established, they are normal EOF.
        if self.established {
            return StartupEioReadiness::EstablishedEof;
        }
        // A process that exits before writing anything is not starting. Each
        // retry costs a scheduler wakeup, so waiting out the whole budget
        // delayed its death by hundreds of milliseconds idle and by seconds
        // on a loaded host.
        if session_leader_exited() {
            return StartupEioReadiness::SessionLeaderExited;
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
    match reader.try_read(buffer) {
        Ok(bytes_read) => Ok(PaneRead::Bytes(bytes_read)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(PaneRead::NotReady),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(PaneRead::NotReady),
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
            Ok(PaneRead::SlaveUnavailable)
        }
        Err(error) => Err(error),
    }
}

async fn write_all_to_stream(stream: &AttachTransport, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(bytes).await
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
        let pane_reader = open_pane_writer(output_reader)?;
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
        let pane_reader = open_pane_writer(output_reader)?;

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

    #[tokio::test]
    async fn read_from_pane_ends_a_silent_exit_without_spending_the_startup_budget(
    ) -> Result<(), Box<dyn Error>> {
        let (mut master, mut child) = ChildCommand::new("sh")
            .args(["-c", "exit 7"])
            .size(PtyTerminalSize::new(80, 24))
            .spawn()?
            .into_parts();
        let pane_reader = open_pane_writer(master.try_clone_for_startup_reader()?)?;
        drop(master);
        // Observed without reaping, as the pane exit handler reaps only after
        // the reader's EOF. Liveness bound only: a loaded host may be slow to
        // run the shell at all.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !pane_reader.get_ref().session_leader_has_exited() {
            assert!(Instant::now() < deadline, "sh -c 'exit 7' never exited");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let mut readiness = PaneReadinessState::default();
        let mut buffer = [0_u8; 256];
        let bytes_read = read_from_pane(&pane_reader, &mut readiness, &mut buffer).await?;
        let status = child.wait()?;

        assert_eq!(bytes_read, 0, "a silent exit has no output to read");
        assert_eq!(
            (
                readiness.startup_eio_reads(),
                readiness.startup_eio_exhausted()
            ),
            (0, false),
            "the EOF of an exited process is not startup latency to retry"
        );
        assert_eq!(status.code(), Some(7));
        Ok(())
    }

    #[tokio::test]
    async fn read_from_pane_ends_a_silent_exit_already_reaped_by_a_status_query(
    ) -> Result<(), Box<dyn Error>> {
        let (mut master, mut child) = ChildCommand::new("sh")
            .args(["-c", "exit 7"])
            .size(PtyTerminalSize::new(80, 24))
            .spawn()?
            .into_parts();
        let pane_reader = open_pane_writer(master.try_clone_for_startup_reader()?)?;
        drop(master);
        // `#{pane_dead}`, send-keys, pipe-pane or a respawn check ask the
        // child handle whether the pane lives, which reaps an exited process
        // before the reader has looked at it. Liveness bound only.
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            assert!(Instant::now() < deadline, "sh -c 'exit 7' never exited");
            tokio::time::sleep(Duration::from_millis(5)).await;
        };

        let mut readiness = PaneReadinessState::default();
        let mut buffer = [0_u8; 256];
        let bytes_read = read_from_pane(&pane_reader, &mut readiness, &mut buffer).await?;

        assert_eq!(bytes_read, 0, "a silent exit has no output to read");
        assert_eq!(
            (
                readiness.startup_eio_reads(),
                readiness.startup_eio_exhausted()
            ),
            (0, false),
            "the EOF of a reaped process is not startup latency to retry"
        );
        assert_eq!(status.code(), Some(7));
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

#[cfg(test)]
mod emit_render_tests {
    use super::{emit_attach_bytes, emit_attach_message, emit_coalescible_render_frame};
    use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
    use crate::pane_io::attach_transport::AttachTransport;
    use rmux_core::OptionStore;
    use rmux_proto::{
        AttachFrameDecoder, AttachMessage, AttachShellCommand, OptionName, ScopeSelector,
        SetOptionMode, DEFAULT_MAX_FRAME_LENGTH,
    };
    use tokio::io::AsyncReadExt;

    async fn emitted_messages(
        outer_terminal: &OuterTerminal,
        render_frame: &[u8],
        render_stream: bool,
        expected_count: usize,
    ) -> Vec<AttachMessage> {
        let capacity = render_frame
            .len()
            .saturating_add(expected_count.saturating_mul(5))
            .saturating_add(64);
        let (stream, mut peer) = tokio::io::duplex(capacity);
        let stream = AttachTransport::from_io(stream);

        emit_coalescible_render_frame(&stream, outer_terminal, render_frame, render_stream)
            .await
            .expect("render frame emits");

        let mut decoder = AttachFrameDecoder::new();
        let mut messages = Vec::with_capacity(expected_count);
        let mut bytes = [0_u8; 8192];
        while messages.len() < expected_count {
            let count = peer.read(&mut bytes).await.expect("read emitted frame");
            assert!(count > 0, "attach stream closed before all frames arrived");
            decoder.push_bytes(&bytes[..count]);
            while let Some(message) = decoder.next_message().expect("decode emitted frame") {
                messages.push(message);
            }
        }
        messages
    }

    async fn emitted_message(render_stream: bool) -> AttachMessage {
        let outer_terminal =
            OuterTerminal::resolve(&OptionStore::default(), OuterTerminalContext::default());
        emitted_messages(&outer_terminal, b"frame", render_stream, 1)
            .await
            .pop()
            .expect("message emitted")
    }

    #[tokio::test]
    async fn coalescible_render_falls_back_to_data_for_legacy_clients() {
        assert!(matches!(
            emitted_message(false).await,
            AttachMessage::Data(bytes) if bytes.ends_with(b"frame")
        ));
    }

    #[tokio::test]
    async fn coalescible_render_uses_render_for_capable_clients() {
        assert!(matches!(
            emitted_message(true).await,
            AttachMessage::Render(bytes) if bytes.ends_with(b"frame")
        ));
    }

    #[tokio::test]
    async fn coalescible_render_fragments_strictly_above_attach_payload_ceiling() {
        let outer_terminal =
            OuterTerminal::resolve(&OptionStore::default(), OuterTerminalContext::default());
        let at_limit = vec![b'a'; DEFAULT_MAX_FRAME_LENGTH];
        let above_limit = vec![b'b'; DEFAULT_MAX_FRAME_LENGTH + 1];

        assert_eq!(
            emitted_messages(&outer_terminal, &at_limit, true, 1).await,
            vec![AttachMessage::Render(at_limit)]
        );
        assert_eq!(
            emitted_messages(&outer_terminal, &above_limit, true, 2).await,
            vec![
                AttachMessage::Data(vec![b'b'; DEFAULT_MAX_FRAME_LENGTH]),
                AttachMessage::Data(vec![b'b']),
            ]
        );
        assert_eq!(
            emitted_messages(&outer_terminal, &above_limit, false, 2).await,
            vec![
                AttachMessage::Data(vec![b'b'; DEFAULT_MAX_FRAME_LENGTH]),
                AttachMessage::Data(vec![b'b']),
            ]
        );
    }

    #[tokio::test]
    async fn fragmented_attach_data_bounds_every_payload_and_preserves_order() {
        let mut payload = (0..(2 * DEFAULT_MAX_FRAME_LENGTH + 17))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let split_sequence = b"\x1b[38;2;1;2;3mX";
        let split_start = DEFAULT_MAX_FRAME_LENGTH - 2;
        payload[split_start..split_start + split_sequence.len()].copy_from_slice(split_sequence);
        let capacity = payload.len() + 3 * 5;
        let (stream, mut peer) = tokio::io::duplex(capacity);
        let stream = AttachTransport::from_io(stream);

        emit_attach_bytes(&stream, &payload)
            .await
            .expect("fragmented data emits");

        let mut decoder = AttachFrameDecoder::new();
        let mut decoded = Vec::with_capacity(payload.len());
        let mut lengths = Vec::new();
        let mut bytes = [0_u8; 8192];
        while decoded.len() < payload.len() {
            let count = peer.read(&mut bytes).await.expect("read emitted data");
            assert!(count > 0, "attach stream closed before all data arrived");
            decoder.push_bytes(&bytes[..count]);
            while let Some(message) = decoder.next_message().expect("decode emitted data") {
                let AttachMessage::Data(bytes) = message else {
                    panic!("fragmented terminal bytes must use strict data frames");
                };
                lengths.push(bytes.len());
                decoded.extend_from_slice(&bytes);
            }
        }

        assert_eq!(
            lengths,
            vec![DEFAULT_MAX_FRAME_LENGTH, DEFAULT_MAX_FRAME_LENGTH, 17]
        );
        assert_eq!(decoded, payload);
    }

    #[tokio::test]
    async fn sync_wrapper_degrades_without_splitting_replaceable_render() {
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Global,
                OptionName::TerminalFeatures,
                "xterm*:sync".to_owned(),
                SetOptionMode::Append,
            )
            .expect("terminal-features append succeeds");
        let outer_terminal = OuterTerminal::resolve(
            &options,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        );
        let sync_overhead = outer_terminal.wrap_render_frame(b"x").len() - 1;
        let at_limit = vec![b'a'; DEFAULT_MAX_FRAME_LENGTH - sync_overhead];
        let above_limit = vec![b'b'; DEFAULT_MAX_FRAME_LENGTH - sync_overhead + 1];

        let exact = emitted_messages(&outer_terminal, &at_limit, true, 1).await;
        let AttachMessage::Render(exact) = &exact[0] else {
            panic!("an exactly bounded synchronized repaint stays replaceable");
        };
        assert_eq!(exact.len(), DEFAULT_MAX_FRAME_LENGTH);
        assert!(exact.starts_with(b"\x1b[?2026h"));
        assert!(exact.ends_with(b"\x1b[?2026l"));

        assert_eq!(
            emitted_messages(&outer_terminal, &above_limit, true, 1).await,
            vec![AttachMessage::Render(above_limit)]
        );
    }

    #[tokio::test]
    async fn attach_control_messages_are_not_wrapped_as_terminal_data() {
        let (stream, mut peer) = tokio::io::duplex(1024);
        let stream = AttachTransport::from_io(stream);
        let message = AttachMessage::DetachExecShellCommand(AttachShellCommand::new(
            "echo detached".to_owned(),
            "/bin/sh".to_owned(),
            "/tmp".to_owned(),
        ));

        emit_attach_message(&stream, &message)
            .await
            .expect("attach message emits");

        let mut bytes = [0_u8; 256];
        let count = peer.read(&mut bytes).await.expect("read emitted frame");
        let mut decoder = AttachFrameDecoder::new();
        decoder.push_bytes(&bytes[..count]);
        assert_eq!(
            decoder.next_message().expect("decode succeeds"),
            Some(message)
        );
    }
}
