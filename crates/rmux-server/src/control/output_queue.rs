use std::collections::{HashSet, VecDeque};
use std::io;
use std::time::{Duration, Instant};

use rmux_proto::{format_exit_line, CONTROL_MAXIMUM_AGE_MS};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use super::pane_event_delivery::PaneEventRetention;
use super::ControlClientFlags;

const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(super) struct ControlBlock {
    bytes: Vec<u8>,
    output: bool,
    pane_id: Option<u32>,
    enqueued_at: Instant,
    _age: Option<PaneEventRetention>,
}

#[derive(Debug, Default)]
pub(super) struct ControlOutputQueue {
    pub(super) blocks: VecDeque<ControlBlock>,
    pub(super) buffered_bytes: usize,
    transport_closed: bool,
    terminal: bool,
}

impl ControlOutputQueue {
    pub(super) fn is_transport_closed(&self) -> bool {
        self.transport_closed
    }

    pub(super) fn enqueue_line(&mut self, mut bytes: Vec<u8>, output: bool) {
        if self.transport_closed {
            return;
        }
        if !output {
            bytes = ensure_control_newline(bytes);
        }
        self.buffered_bytes = self.buffered_bytes.saturating_add(bytes.len());
        self.blocks.push_back(ControlBlock {
            bytes,
            output,
            pane_id: None,
            enqueued_at: Instant::now(),
            _age: None,
        });
    }

    pub(super) fn enqueue_exit(&mut self, reason: Option<&str>) {
        self.enqueue_line(format_exit_line(reason).into_bytes(), false);
        self.terminal = true;
    }

    pub(super) fn enqueue_pane_output(
        &mut self,
        pane_id: u32,
        bytes: Vec<u8>,
        age: Option<PaneEventRetention>,
    ) {
        if self.transport_closed {
            return;
        }
        self.buffered_bytes = self.buffered_bytes.saturating_add(bytes.len());
        self.blocks.push_back(ControlBlock {
            bytes,
            output: true,
            pane_id: Some(pane_id),
            enqueued_at: Instant::now(),
            _age: age,
        });
    }

    pub(super) fn enqueue_stdout(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.enqueue_line(bytes, false);
        }
    }

    pub(super) fn discard_pane_output(&mut self, pane_id: u32) {
        self.blocks.retain(|block| {
            let discard = block.output && block.pane_id == Some(pane_id);
            if discard {
                self.buffered_bytes = self.buffered_bytes.saturating_sub(block.bytes.len());
            }
            !discard
        });
    }

    pub(super) fn oldest_pane_output_age(&self, pane_id: u32) -> Option<Duration> {
        self.blocks
            .iter()
            .find(|block| block.output && block.pane_id == Some(pane_id))
            .map(|block| block.enqueued_at.elapsed())
    }
}

pub(super) async fn flush_output_queue(
    output_queue: &mut ControlOutputQueue,
    writer: &mut (impl AsyncWrite + Unpin),
    flags: ControlClientFlags,
    paused_panes: &mut HashSet<u32>,
    mut pane_events: Option<(
        &mut tokio::sync::mpsc::Receiver<super::PaneEvent>,
        &mut super::ControlPaneOutputStates,
    )>,
) -> io::Result<()> {
    if output_queue.transport_closed {
        return Ok(());
    }
    let result = async {
        // Hand off a ready pane drain once after its first complete write.
        #[cfg(unix)]
        let mut handoff_pending = true;
        let mut lagged = false;
        while let Some(block) = output_queue.blocks.pop_front() {
            if output_queue.terminal {
                pane_events = None;
            }
            if block.output
                && !flags.uses_extended_output()
                && block.enqueued_at.elapsed() > Duration::from_millis(CONTROL_MAXIMUM_AGE_MS)
            {
                write_all_bounded(writer, format_exit_line(Some("too far behind")).as_bytes())
                    .await?;
                flush_bounded(writer).await?;
                return Err(io::Error::other("too far behind"));
            }
            let write = write_all_bounded(writer, &block.bytes);
            tokio::pin!(write);
            if let Some((events, states)) = pane_events.as_mut() {
                // Queue copies during the write, retaining age and releasing copy permits.
                loop {
                    tokio::select! {
                        biased;
                        result = &mut write => { result?; break; }
                        Some(event) = events.recv(), if !lagged => {
                            lagged = super::handle_pane_event(
                                event, output_queue, paused_panes, states, flags,
                            )?;
                        }
                    }
                }
            } else {
                write.await?;
            }
            output_queue.buffered_bytes -= block.bytes.len();
            if output_queue.terminal {
                pane_events = None;
            }
            if let Some((events, states)) = pane_events.as_mut() {
                #[cfg(unix)]
                if block.pane_id.is_some() {
                    drop(block._age);
                    if std::mem::take(&mut handoff_pending) {
                        tokio::task::yield_now().await;
                    }
                }
                lagged = lagged
                    || super::subscriptions::drain_channel_pane_events(
                        events,
                        output_queue,
                        paused_panes,
                        states,
                        flags,
                    )?;
            }
        }
        flush_bounded(writer).await?;
        if lagged {
            Err(io::Error::other("too far behind"))
        } else {
            Ok(())
        }
    }
    .await;
    result.or_else(|error| finish_after_write_error(output_queue, paused_panes, error))
}

fn finish_after_write_error(
    output_queue: &mut ControlOutputQueue,
    paused_panes: &mut HashSet<u32>,
    error: io::Error,
) -> io::Result<()> {
    #[cfg(windows)]
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    ) {
        output_queue.transport_closed = true;
        output_queue.blocks.clear();
        output_queue.buffered_bytes = 0;
        paused_panes.clear();
        return Ok(());
    }

    let _ = (output_queue, paused_panes);
    Err(error)
}

pub(super) async fn write_all_bounded(
    writer: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
) -> io::Result<()> {
    timeout(CONTROL_WRITE_TIMEOUT, writer.write_all(bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "control client write timed out"))?
}

pub(super) async fn flush_bounded(writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
    timeout(CONTROL_WRITE_TIMEOUT, flush_transport(writer))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "control client flush timed out"))?
}

async fn flush_transport(writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
    writer.flush().await?;
    await_platform_write_completion(writer).await
}

#[cfg(not(windows))]
async fn await_platform_write_completion(writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
    let _ = writer;
    Ok(())
}

#[cfg(any(windows, test))]
async fn await_pending_overlapped_write(writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
    // Tokio 1.52's named-pipe flush is a no-op, while Mio 1.2 may report the
    // entire input as written as soon as its overlapped WriteFile is submitted.
    // A second poll_write cannot be accepted until that operation's completion
    // has been processed. Poll an empty write directly (write_all would skip an
    // empty slice) to cross that boundary without adding a protocol byte.
    let written = writer.write(&[]).await?;
    if written == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("zero-byte named-pipe write barrier reported {written} bytes"),
        ))
    }
}

#[cfg(windows)]
async fn await_platform_write_completion(writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
    await_pending_overlapped_write(writer).await
}

pub(super) fn ensure_control_newline(mut bytes: Vec<u8>) -> Vec<u8> {
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    use super::*;

    struct OverlappedWriteModel {
        state: Arc<Mutex<WriteState>>,
    }

    #[derive(Default)]
    struct WriteState {
        submitted: bool,
        completed: bool,
        barrier_waker: Option<Waker>,
    }

    impl OverlappedWriteModel {
        fn new() -> (Self, Arc<Mutex<WriteState>>) {
            let state = Arc::new(Mutex::new(WriteState::default()));
            (
                Self {
                    state: Arc::clone(&state),
                },
                state,
            )
        }
    }

    impl AsyncWrite for OverlappedWriteModel {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let mut state = self.state.lock().expect("overlapped write model");
            if !bytes.is_empty() {
                assert!(!state.submitted, "only one payload write is expected");
                state.submitted = true;
                // Match Mio: the owned buffer was submitted to IOCP, so the
                // caller sees the full input length before completion.
                return Poll::Ready(Ok(bytes.len()));
            }
            assert!(state.submitted, "the barrier follows a payload write");
            if state.completed {
                Poll::Ready(Ok(0))
            } else {
                state.barrier_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            // Match Tokio's named-pipe implementation.
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.poll_flush(cx)
        }
    }

    #[tokio::test]
    async fn windows_flush_waits_for_the_prior_overlapped_write_completion() {
        let (mut writer, state) = OverlappedWriteModel::new();
        writer
            .write_all(b"terminal control output")
            .await
            .expect("submit modeled overlapped write");

        // The parent behavior trusted this no-op and allowed the drain to race
        // ahead of the payload operation.
        writer.flush().await.expect("modeled Tokio flush");

        let mut barrier = Box::pin(await_pending_overlapped_write(&mut writer));
        assert!(
            timeout(Duration::from_millis(25), barrier.as_mut())
                .await
                .is_err(),
            "the Windows flush must not overtake the pending write"
        );

        let waker = {
            let mut state = state.lock().expect("overlapped write model");
            state.completed = true;
            state.barrier_waker.take()
        };
        waker.expect("the barrier registered a waker").wake();
        timeout(Duration::from_secs(1), barrier)
            .await
            .expect("the barrier must observe write completion")
            .expect("the zero-byte write barrier succeeds");
    }
}

#[cfg(test)]
#[path = "output_queue/slow_reader_tests.rs"]
mod slow_reader_tests;
