//! Attach-stream output loop: frame decoding, render coalescing and
//! termination-aware stream reads.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use rmux_proto::{decode_attach_data_frame, AttachFrameDecoder, AttachMessage, RmuxError};
use rustix::event::{poll, PollFd, PollFlags};

use crate::attach_lock_state::AttachLockState;
use crate::ClientError;

use super::actions::{send_attach_action, ClientAttachAction, ClientAttachEvent};
use super::render_drain::{drain_available_attach_stream, flush_pending_render};
use super::screen::{AttachScreenTracker, AttachStopDetector};
use super::{termination, POLL_TIMEOUT, READ_BUFFER_SIZE};

const RENDER_MAX_PENDING: Duration = Duration::from_millis(8);

#[cfg(test)]
pub(super) fn output_loop<Output>(
    stream: UnixStream,
    initial_bytes: Vec<u8>,
    output: Output,
    closed: Arc<AtomicBool>,
    locked: Arc<AttachLockState>,
    screen_tracker: AttachScreenTracker,
    event_tx: mpsc::Sender<ClientAttachEvent>,
) -> std::result::Result<(), ClientError>
where
    Output: Write,
{
    output_loop_with_termination(
        stream,
        initial_bytes,
        output,
        closed,
        locked,
        screen_tracker,
        event_tx,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn output_loop_with_termination<Output>(
    mut stream: UnixStream,
    initial_bytes: Vec<u8>,
    output: Output,
    closed: Arc<AtomicBool>,
    locked: Arc<AttachLockState>,
    screen_tracker: AttachScreenTracker,
    event_tx: mpsc::Sender<ClientAttachEvent>,
    termination_signals_enabled: bool,
) -> std::result::Result<(), ClientError>
where
    Output: Write,
{
    let mut output = termination::TerminationAwareWriter::new(output, termination_signals_enabled);
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&initial_bytes);
    let mut read_buffer = [0_u8; READ_BUFFER_SIZE];
    let mut stop_detector = AttachStopDetector::new(screen_tracker.clone());
    let mut pending_render = None::<Vec<u8>>;
    let mut pending_render_started_at = None::<Instant>;
    let mut pending_render_drained_after_deadline = false;
    let mut painted_render_frame = false;
    let mut data_scratch = [0_u8; READ_BUFFER_SIZE];

    loop {
        fail_if_attach_termination_requested(termination_signals_enabled)?;
        loop {
            while let Some(bytes) = decoder
                .next_data_payload_into(&mut data_scratch)
                .map_err(ClientError::from)?
            {
                flush_pending_render_state(
                    &mut output,
                    &mut pending_render,
                    &mut pending_render_started_at,
                )?;
                handle_attach_data_payload(&mut output, &locked, &mut stop_detector, bytes)?;
            }

            let Some(message) = decoder.next_message().map_err(ClientError::from)? else {
                break;
            };
            match message {
                AttachMessage::Data(bytes) => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    handle_attach_data_payload(&mut output, &locked, &mut stop_detector, &bytes)?;
                }
                AttachMessage::Render(bytes) => {
                    if locked.is_locked() {
                        continue;
                    }
                    if pending_render.is_none() {
                        pending_render_started_at = Some(Instant::now());
                        pending_render_drained_after_deadline = false;
                    }
                    pending_render = Some(bytes);
                    if !painted_render_frame
                        && flush_pending_render_state(
                            &mut output,
                            &mut pending_render,
                            &mut pending_render_started_at,
                        )?
                    {
                        pending_render_drained_after_deadline = false;
                        painted_render_frame = true;
                    }
                }
                AttachMessage::KeyDispatched(_) => {}
                AttachMessage::Resize(_) | AttachMessage::ResizeGeometry(_) => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    return Err(ClientError::Protocol(RmuxError::Decode(
                        "received unexpected resize message from attach stream".to_owned(),
                    )));
                }
                AttachMessage::Lock(command) => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    locked.lock();
                    send_attach_action(
                        &event_tx,
                        ClientAttachAction::Lock {
                            command,
                            stop_generation: screen_tracker.current_stop_generation(),
                        },
                    )?;
                }
                AttachMessage::LockShellCommand(command) => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    locked.lock();
                    send_attach_action(
                        &event_tx,
                        ClientAttachAction::LockShell {
                            command,
                            stop_generation: screen_tracker.current_stop_generation(),
                        },
                    )?;
                }
                AttachMessage::Suspend => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    locked.lock();
                    send_attach_action(
                        &event_tx,
                        ClientAttachAction::Suspend {
                            stop_generation: screen_tracker.current_stop_generation(),
                        },
                    )?;
                }
                AttachMessage::DetachKill => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    closed.store(true, Ordering::SeqCst);
                    send_attach_action(&event_tx, ClientAttachAction::DetachKill)?;
                    return Ok(());
                }
                AttachMessage::DetachExec(command) => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    closed.store(true, Ordering::SeqCst);
                    send_attach_action(&event_tx, ClientAttachAction::DetachExec(command))?;
                    return Ok(());
                }
                AttachMessage::DetachExecShellCommand(command) => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    closed.store(true, Ordering::SeqCst);
                    send_attach_action(&event_tx, ClientAttachAction::DetachExecShell(command))?;
                    return Ok(());
                }
                AttachMessage::ServerExited => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    closed.store(true, Ordering::SeqCst);
                    return Err(ClientError::ServerExited);
                }
                AttachMessage::Unlock => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    return Err(ClientError::Protocol(RmuxError::Decode(
                        "received unexpected unlock message from attach stream".to_owned(),
                    )));
                }
                AttachMessage::Keystroke(_) => {
                    flush_pending_render_state(
                        &mut output,
                        &mut pending_render,
                        &mut pending_render_started_at,
                    )?;
                    return Err(ClientError::Protocol(RmuxError::Decode(
                        "received unexpected keystroke message from attach stream".to_owned(),
                    )));
                }
            }
        }

        if pending_render.is_some() {
            let pending_expired = pending_render_expired(pending_render_started_at);
            if (!pending_expired || !pending_render_drained_after_deadline)
                && drain_available_attach_stream(&mut stream, &mut decoder, &mut read_buffer)?
            {
                if pending_expired {
                    pending_render_drained_after_deadline = true;
                }
                continue;
            }
        }
        if pending_render.is_some() && !pending_render_expired(pending_render_started_at) {
            sleep_until_pending_render_deadline(pending_render_started_at);
            if drain_available_attach_stream(&mut stream, &mut decoder, &mut read_buffer)? {
                continue;
            }
        }
        if flush_pending_render_state(
            &mut output,
            &mut pending_render,
            &mut pending_render_started_at,
        )? {
            pending_render_drained_after_deadline = false;
            painted_render_frame = true;
        }

        let read_result =
            read_attach_stream(&mut stream, &mut read_buffer, termination_signals_enabled);
        let bytes_read = match read_result {
            Ok(0) => {
                closed.store(true, Ordering::SeqCst);
                if screen_tracker.was_stopped() {
                    return Ok(());
                }
                return Err(ClientError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "attach stream closed before attach-stop sequence",
                )));
            }
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if screen_tracker.was_stopped()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                    ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(ClientError::Io(error)),
        };

        let mut consumed = 0;
        if decoder.is_empty() {
            while consumed < bytes_read {
                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..bytes_read])
                    .map_err(ClientError::from)?
                else {
                    break;
                };
                handle_attach_data_payload(
                    &mut output,
                    &locked,
                    &mut stop_detector,
                    frame.payload(),
                )?;
                consumed += frame.frame_len();
            }
        }
        if consumed < bytes_read {
            decoder.push_bytes(&read_buffer[consumed..bytes_read]);
        }
    }
}

fn read_attach_stream(
    stream: &mut UnixStream,
    read_buffer: &mut [u8],
    termination_signals_enabled: bool,
) -> io::Result<usize> {
    if !termination_signals_enabled {
        return stream.read(read_buffer);
    }

    loop {
        if termination::was_requested() {
            return Err(termination::interruption_error());
        }
        let mut fds = [PollFd::new(
            &*stream,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        match poll(&mut fds, Some(&POLL_TIMEOUT)) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
        if termination::was_requested() {
            return Err(termination::interruption_error());
        }
        if fds[0].revents().is_empty() {
            continue;
        }
        return stream.read(read_buffer);
    }
}

fn fail_if_attach_termination_requested(
    termination_signals_enabled: bool,
) -> std::result::Result<(), ClientError> {
    if termination_signals_enabled && termination::was_requested() {
        return Err(ClientError::Io(termination::interruption_error()));
    }
    Ok(())
}

fn handle_attach_data_payload<Output>(
    output: &mut Output,
    locked: &Arc<AttachLockState>,
    stop_detector: &mut AttachStopDetector,
    bytes: &[u8],
) -> std::result::Result<(), ClientError>
where
    Output: Write,
{
    stop_detector.observe(bytes);
    if locked.is_locked() {
        return Ok(());
    }
    output.write_all(bytes).map_err(ClientError::Io)?;
    output.flush().map_err(ClientError::Io)?;
    Ok(())
}

fn pending_render_expired(started_at: Option<Instant>) -> bool {
    started_at.is_some_and(|started_at| started_at.elapsed() >= RENDER_MAX_PENDING)
}

fn sleep_until_pending_render_deadline(started_at: Option<Instant>) {
    let Some(started_at) = started_at else {
        return;
    };
    let Some(remaining) = RENDER_MAX_PENDING.checked_sub(started_at.elapsed()) else {
        return;
    };
    if !remaining.is_zero() {
        thread::sleep(remaining);
    }
}

fn flush_pending_render_state<Output>(
    output: &mut Output,
    pending_render: &mut Option<Vec<u8>>,
    pending_render_started_at: &mut Option<Instant>,
) -> std::result::Result<bool, ClientError>
where
    Output: Write,
{
    let flushed = pending_render.is_some();
    flush_pending_render(output, pending_render)?;
    *pending_render_started_at = None;
    Ok(flushed)
}
