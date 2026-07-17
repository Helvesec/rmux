//! Raw terminal lifecycle and attach-stream helpers for attach-mode clients.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use rmux_proto::{
    decode_attach_data_frame, encode_attach_data, encode_attach_data_into_slice,
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachShellCommand, RmuxError,
    TerminalGeometry, TerminalSize, ATTACH_DATA_HEADER_LEN,
};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::process::{kill_process, Signal};

use crate::attach_lock_state::AttachLockState;
use crate::ClientError;

#[path = "attach/render_drain.rs"]
mod render_drain;
#[path = "attach/resize.rs"]
mod resize;
#[path = "attach/screen.rs"]
mod screen;
#[path = "attach/terminal.rs"]
mod terminal;
#[path = "attach/terminal_cleanup.rs"]
mod terminal_cleanup;
#[path = "attach/termination.rs"]
mod termination;

use render_drain::{drain_available_attach_stream, flush_pending_render};
#[cfg(test)]
use resize::terminal_size_from_fd;
use resize::{terminal_geometry_from_fd, ResizeWatcher, SignalMaskGuard};
use screen::{AttachScreenTracker, AttachStopDetector, AttachStopGeneration};
use terminal::current_process_pid;
pub use terminal::{AttachError, RawTerminal, Result};
use termination::AttachTerminationGuard;

#[cfg(test)]
use terminal_cleanup::fallback_attach_stop_sequence;

const READ_BUFFER_SIZE: usize = 8192;
const STACK_ATTACH_DATA_PAYLOAD: usize = 1024;
const POLL_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 100_000_000,
};
const RENDER_MAX_PENDING: Duration = Duration::from_millis(8);
const TERMINATION_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TERMINATION_OUTPUT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);

/// Runs the attach loop using the process stdin/stdout streams.
pub fn attach_terminal(stream: UnixStream) -> std::result::Result<(), ClientError> {
    attach_terminal_with_initial_bytes(stream, Vec::new())
}

/// Runs the attach loop using process stdin/stdout and pre-read stream bytes.
pub fn attach_terminal_with_initial_bytes(
    stream: UnixStream,
    initial_bytes: Vec<u8>,
) -> std::result::Result<(), ClientError> {
    attach_terminal_with_initial_bytes_and_geometry_flag(stream, initial_bytes, false)
}

/// Runs the attach loop and sends resize events with pixel geometry.
///
/// Call this only after the daemon advertises the
/// `stream.attach.resize_geometry` capability. Older daemons do not understand
/// that attach-stream frame and would close the stream on decode.
pub fn attach_terminal_with_initial_bytes_and_resize_geometry(
    stream: UnixStream,
    initial_bytes: Vec<u8>,
) -> std::result::Result<(), ClientError> {
    attach_terminal_with_initial_bytes_and_geometry_flag(stream, initial_bytes, true)
}

fn attach_terminal_with_initial_bytes_and_geometry_flag(
    stream: UnixStream,
    initial_bytes: Vec<u8>,
    resize_geometry_enabled: bool,
) -> std::result::Result<(), ClientError> {
    let terminal = io::stdin();
    let input = io::stdin();
    let output = File::from(
        io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map_err(AttachError::from)?,
    );

    attach_with_terminal_with_initial_bytes(
        stream,
        initial_bytes,
        &terminal,
        input,
        output,
        resize_geometry_enabled,
    )
}

/// Runs the attach loop with an explicit terminal file descriptor.
///
/// The `terminal` handle is used for raw-mode lifecycle and resize discovery,
/// while `input` and `output` carry the byte stream.
pub fn attach_with_terminal<Terminal, Input, Output>(
    stream: UnixStream,
    terminal: &Terminal,
    input: Input,
    output: Output,
) -> std::result::Result<(), ClientError>
where
    Terminal: AsFd,
    Input: Read + AsFd + Send + 'static,
    Output: Write + Send + 'static,
{
    attach_with_terminal_with_initial_bytes(stream, Vec::new(), terminal, input, output, false)
}

fn attach_with_terminal_with_initial_bytes<Terminal, Input, Output>(
    stream: UnixStream,
    initial_bytes: Vec<u8>,
    terminal: &Terminal,
    input: Input,
    output: Output,
    resize_geometry_enabled: bool,
) -> std::result::Result<(), ClientError>
where
    Terminal: AsFd,
    Input: Read + AsFd + Send + 'static,
    Output: Write + Send + 'static,
{
    let termination_guard = AttachTerminationGuard::install().map_err(ClientError::Io)?;
    let raw_terminal = RawTerminal::from_fd(terminal).map_err(ClientError::from)?;
    let _ = raw_terminal.flush_pending_input();
    let screen_tracker = AttachScreenTracker::default();
    let attach_state = AttachTerminalState {
        stream,
        initial_bytes,
        terminal,
        raw_terminal: &raw_terminal,
        screen_tracker: &screen_tracker,
        resize_geometry_enabled,
        termination_signals_enabled: true,
    };
    let result = drive_attach_with_terminal_state(attach_state, input, output);
    if result.is_err() {
        if termination::was_requested() {
            let _ = raw_terminal.restore_after_termination();
        } else if !screen_tracker.was_stopped() {
            let _ = raw_terminal.restore_attach_terminal_state();
        }
    }
    let _ = raw_terminal.flush_pending_input();
    drop(raw_terminal);
    termination_guard.finish().map_err(ClientError::Io)?;
    result
}

struct AttachTerminalState<'a, Terminal> {
    stream: UnixStream,
    initial_bytes: Vec<u8>,
    terminal: &'a Terminal,
    raw_terminal: &'a RawTerminal,
    screen_tracker: &'a AttachScreenTracker,
    resize_geometry_enabled: bool,
    termination_signals_enabled: bool,
}

struct AttachStreamState<'a> {
    stream: UnixStream,
    initial_bytes: Vec<u8>,
    raw_terminal: Option<&'a RawTerminal>,
    screen_tracker: AttachScreenTracker,
    resize_events: mpsc::Receiver<TerminalGeometry>,
    resize_geometry_enabled: bool,
    termination_signals_enabled: bool,
}

struct AttachInputReadLease<'a> {
    state: &'a AttachLockState,
}

impl<'a> AttachInputReadLease<'a> {
    fn acquire(state: &'a AttachLockState) -> Option<Self> {
        state.begin_input_read().then_some(Self { state })
    }
}

impl Drop for AttachInputReadLease<'_> {
    fn drop(&mut self) {
        self.state.finish_input_read();
    }
}

fn drive_attach_with_terminal_state<Terminal, Input, Output>(
    state: AttachTerminalState<'_, Terminal>,
    input: Input,
    output: Output,
) -> std::result::Result<(), ClientError>
where
    Terminal: AsFd,
    Input: Read + AsFd + Send + 'static,
    Output: Write + Send + 'static,
{
    // This helper runs while the caller's `RawTerminal` guard is still alive,
    // which keeps termios restoration as the last drop on every return path.
    let _signal_mask = SignalMaskGuard::block_winch().map_err(ClientError::from)?;
    let (resize_tx, resize_rx) = mpsc::channel();
    let initial_geometry = terminal_geometry_from_fd(state.terminal).map_err(ClientError::from)?;
    let terminal_fd = state
        .terminal
        .as_fd()
        .try_clone_to_owned()
        .map_err(AttachError::from)?;

    if let Some(initial_geometry) = initial_geometry {
        resize_tx.send(initial_geometry).map_err(|_| {
            ClientError::Io(io::Error::other(
                "resize channel closed before attach start",
            ))
        })?;
    }

    let resize_watcher = ResizeWatcher::spawn(terminal_fd, resize_tx)?;
    let stream_state = AttachStreamState {
        stream: state.stream,
        initial_bytes: state.initial_bytes,
        raw_terminal: Some(state.raw_terminal),
        screen_tracker: state.screen_tracker.clone(),
        resize_events: resize_rx,
        resize_geometry_enabled: state.resize_geometry_enabled,
        termination_signals_enabled: state.termination_signals_enabled,
    };
    let attach_result = drive_attach_stream_inner(stream_state, input, output);
    drop(resize_watcher);
    attach_result
}

/// Drives raw attach-stream byte forwarding over an upgraded Unix socket.
pub fn drive_attach_stream<Input, Output>(
    stream: UnixStream,
    input: Input,
    output: Output,
    resize_events: mpsc::Receiver<TerminalSize>,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsFd + Send + 'static,
    Output: Write + Send + 'static,
{
    let resize_events = geometry_resize_events_from_size_events(resize_events);
    let stream_state = AttachStreamState {
        stream,
        initial_bytes: Vec::new(),
        raw_terminal: None,
        screen_tracker: AttachScreenTracker::default(),
        resize_events,
        resize_geometry_enabled: false,
        termination_signals_enabled: false,
    };
    drive_attach_stream_inner(stream_state, input, output)
}

fn drive_attach_stream_inner<Input, Output>(
    state: AttachStreamState<'_>,
    input: Input,
    output: Output,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsFd + Send + 'static,
    Output: Write + Send + 'static,
{
    let control = state.stream.try_clone().map_err(ClientError::Io)?;
    let mut lock_stream = state.stream.try_clone().map_err(ClientError::Io)?;
    let input_stream = state.stream.try_clone().map_err(ClientError::Io)?;
    let (input_wakeup, wake_input_thread) = UnixStream::pair().map_err(ClientError::Io)?;
    let closed = Arc::new(AtomicBool::new(false));
    let input_closed = Arc::clone(&closed);
    let output_closed = Arc::clone(&closed);
    let locked = Arc::new(AttachLockState::default());
    let input_locked = Arc::clone(&locked);
    let output_locked = Arc::clone(&locked);
    let (event_tx, event_rx) = mpsc::channel();

    let input_thread = thread::spawn(move || {
        input_loop(
            input_stream,
            input,
            state.resize_events,
            state.resize_geometry_enabled,
            input_closed,
            input_locked,
            wake_input_thread,
        )
    });
    let output_screen_tracker = state.screen_tracker.clone();
    let action_screen_tracker = state.screen_tracker.clone();
    let output_thread = thread::spawn(move || {
        let result = output_loop_with_termination(
            state.stream,
            state.initial_bytes,
            output,
            output_closed,
            output_locked,
            output_screen_tracker,
            event_tx.clone(),
            state.termination_signals_enabled,
        );
        let _ = event_tx.send(ClientAttachEvent::OutputDone);
        result
    });

    let output_result = wait_for_output_thread(
        output_thread,
        state.raw_terminal,
        &mut lock_stream,
        &locked,
        &action_screen_tracker,
        event_rx,
        state.termination_signals_enabled,
    );
    locked.close();
    closed.store(true, Ordering::SeqCst);
    let _ = control.shutdown(Shutdown::Both);
    let _ = input_wakeup.shutdown(Shutdown::Both);
    let input_result = join_attach_thread(input_thread)?;

    let output_result = output_result?;
    output_result?;
    input_result
}

fn geometry_resize_events_from_size_events(
    resize_events: mpsc::Receiver<TerminalSize>,
) -> mpsc::Receiver<TerminalGeometry> {
    let (geometry_tx, geometry_rx) = mpsc::channel();
    let _forwarder = thread::spawn(move || {
        while let Ok(size) = resize_events.recv() {
            if geometry_tx.send(TerminalGeometry::from_size(size)).is_err() {
                break;
            }
        }
    });
    geometry_rx
}

fn input_loop<Input>(
    mut stream: UnixStream,
    mut input: Input,
    resize_events: mpsc::Receiver<TerminalGeometry>,
    resize_geometry_enabled: bool,
    closed: Arc<AtomicBool>,
    locked: Arc<AttachLockState>,
    wakeup: UnixStream,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsFd,
{
    let mut read_buffer = [0_u8; READ_BUFFER_SIZE];

    loop {
        if closed.load(Ordering::SeqCst) {
            return Ok(());
        }

        drain_resize_events(&mut stream, &resize_events, resize_geometry_enabled)?;
        if locked.is_locked() {
            thread::sleep(Duration::from_millis(20));
            continue;
        }

        let mut fds = [
            PollFd::new(&input, PollFlags::IN | PollFlags::ERR | PollFlags::HUP),
            PollFd::new(&wakeup, PollFlags::IN | PollFlags::ERR | PollFlags::HUP),
        ];
        match poll(&mut fds, Some(&POLL_TIMEOUT)) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(ClientError::Io(error.into())),
        }

        if !fds[1].revents().is_empty() {
            return Ok(());
        }

        // The server can request a lock or suspend while this thread is
        // asleep in poll. Recheck after the wakeup and before touching the
        // shared terminal input so the lock command remains the sole reader.
        if locked.is_locked() {
            continue;
        }

        let ready = fds[0].revents();
        if ready.is_empty() {
            continue;
        }
        if closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        if !ready.contains(PollFlags::IN) {
            if ready.contains(PollFlags::HUP) || ready.contains(PollFlags::ERR) {
                shutdown_attach_writes(&stream)?;
                return Ok(());
            }
            continue;
        }

        let Some(_input_read_lease) = AttachInputReadLease::acquire(&locked) else {
            continue;
        };

        let bytes_read = match input.read(&mut read_buffer) {
            Ok(0) => {
                shutdown_attach_writes(&stream)?;
                return Ok(());
            }
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ClientError::Io(error)),
        };

        write_attach_data(&mut stream, &read_buffer[..bytes_read])?;
    }
}

#[cfg(test)]
fn output_loop<Output>(
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
fn output_loop_with_termination<Output>(
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
                let Some(frame) = decode_attach_data_frame(&read_buffer[consumed..])
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

fn wait_for_output_thread(
    output_thread: thread::JoinHandle<std::result::Result<(), ClientError>>,
    raw_terminal: Option<&RawTerminal>,
    lock_stream: &mut UnixStream,
    locked: &Arc<AttachLockState>,
    screen_tracker: &AttachScreenTracker,
    event_rx: mpsc::Receiver<ClientAttachEvent>,
    termination_signals_enabled: bool,
) -> std::result::Result<std::result::Result<(), ClientError>, ClientError> {
    if termination_signals_enabled {
        loop {
            if termination::was_requested() {
                wait_for_backpressured_output_shutdown(&output_thread, raw_terminal, &event_rx);
                return Err(ClientError::Io(termination::interruption_error()));
            }
            match event_rx.recv_timeout(TERMINATION_POLL_INTERVAL) {
                Ok(ClientAttachEvent::Action(action)) => {
                    handle_attach_action(
                        raw_terminal,
                        lock_stream,
                        locked,
                        screen_tracker,
                        action,
                    )?;
                }
                Ok(ClientAttachEvent::OutputDone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    } else {
        while let Ok(ClientAttachEvent::Action(action)) = event_rx.recv() {
            handle_attach_action(raw_terminal, lock_stream, locked, screen_tracker, action)?;
        }
    }

    while let Ok(event) = event_rx.try_recv() {
        match event {
            ClientAttachEvent::Action(action) => {
                handle_attach_action(raw_terminal, lock_stream, locked, screen_tracker, action)?;
            }
            ClientAttachEvent::OutputDone => {}
        }
    }

    join_attach_thread(output_thread)
}

fn wait_for_backpressured_output_shutdown(
    output_thread: &thread::JoinHandle<std::result::Result<(), ClientError>>,
    raw_terminal: Option<&RawTerminal>,
    event_rx: &mpsc::Receiver<ClientAttachEvent>,
) {
    termination::interrupt_thread(output_thread);
    let _nonblocking_output =
        raw_terminal.and_then(|raw_terminal| raw_terminal.interrupt_output_writer().ok());
    let deadline = Instant::now() + TERMINATION_OUTPUT_SHUTDOWN_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match event_rx.recv_timeout(remaining) {
            Ok(ClientAttachEvent::OutputDone) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(ClientAttachEvent::Action(_)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => break,
        }
    }
}

fn send_attach_action(
    event_tx: &mpsc::Sender<ClientAttachEvent>,
    action: ClientAttachAction,
) -> std::result::Result<(), ClientError> {
    event_tx
        .send(ClientAttachEvent::Action(action))
        .map_err(|_| ClientError::Io(io::Error::other("attach event receiver closed")))
}

fn handle_attach_action(
    raw_terminal: Option<&RawTerminal>,
    lock_stream: &mut UnixStream,
    locked: &Arc<AttachLockState>,
    screen_tracker: &AttachScreenTracker,
    action: ClientAttachAction,
) -> std::result::Result<(), ClientError> {
    match action {
        ClientAttachAction::Lock {
            command,
            stop_generation,
        } => {
            locked.wait_until_input_idle();
            let Some(raw_terminal) = raw_terminal else {
                locked.unlock();
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected lock request without a managed terminal".to_owned(),
                )));
            };
            let result = raw_terminal
                .run_lock_command(&command)
                .map_err(ClientError::from)
                .and_then(|()| write_attach_unlock(lock_stream, screen_tracker, stop_generation));
            locked.unlock();
            result
        }
        ClientAttachAction::LockShell {
            command,
            stop_generation,
        } => {
            locked.wait_until_input_idle();
            let Some(raw_terminal) = raw_terminal else {
                locked.unlock();
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected lock request without a managed terminal".to_owned(),
                )));
            };
            let result = raw_terminal
                .run_lock_shell_command(&command)
                .map_err(ClientError::from)
                .and_then(|()| write_attach_unlock(lock_stream, screen_tracker, stop_generation));
            locked.unlock();
            result
        }
        ClientAttachAction::Suspend { stop_generation } => {
            locked.wait_until_input_idle();
            let Some(raw_terminal) = raw_terminal else {
                locked.unlock();
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected suspend request without a managed terminal".to_owned(),
                )));
            };
            let result = raw_terminal
                .suspend_self()
                .map_err(ClientError::from)
                .and_then(|()| write_attach_unlock(lock_stream, screen_tracker, stop_generation));
            locked.unlock();
            result
        }
        ClientAttachAction::DetachKill => {
            if let Some(raw_terminal) = raw_terminal {
                raw_terminal.restore().map_err(ClientError::from)?;
            }
            kill_process(current_process_pid().map_err(ClientError::Io)?, Signal::HUP)
                .map_err(|error| ClientError::Io(error.into()))?;
            Ok(())
        }
        ClientAttachAction::DetachExec(command) => {
            let Some(raw_terminal) = raw_terminal else {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected detach exec request without a managed terminal".to_owned(),
                )));
            };
            raw_terminal
                .run_detach_exec_command(&command)
                .map_err(ClientError::from)
        }
        ClientAttachAction::DetachExecShell(command) => {
            let Some(raw_terminal) = raw_terminal else {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected detach exec request without a managed terminal".to_owned(),
                )));
            };
            raw_terminal
                .run_detach_exec_shell_command(&command)
                .map_err(ClientError::from)
        }
    }
}

fn drain_resize_events(
    stream: &mut UnixStream,
    resize_events: &mpsc::Receiver<TerminalGeometry>,
    resize_geometry_enabled: bool,
) -> std::result::Result<(), ClientError> {
    while let Ok(geometry) = resize_events.try_recv() {
        let message = if resize_geometry_enabled && geometry.pixels.is_some() {
            AttachMessage::ResizeGeometry(geometry)
        } else {
            AttachMessage::Resize(geometry.size)
        };
        write_attach_message(stream, message)?;
    }

    Ok(())
}

fn write_attach_message(
    stream: &mut UnixStream,
    message: AttachMessage,
) -> std::result::Result<(), ClientError> {
    let frame = encode_attach_message(&message).map_err(ClientError::from)?;
    stream.write_all(&frame).map_err(ClientError::Io)
}

fn write_attach_unlock(
    stream: &mut UnixStream,
    screen_tracker: &AttachScreenTracker,
    stop_generation: Option<AttachStopGeneration>,
) -> std::result::Result<(), ClientError> {
    // Rearm only the stop published by this lock/suspend prelude. A newer
    // generation belongs to a concurrent detach or session exit and must stay
    // authoritative even when the local action completes later.
    let rearmed =
        stop_generation.is_some_and(|generation| screen_tracker.rearm_if_current(generation));
    if !rearmed && screen_tracker.was_stopped() {
        return Ok(());
    }

    match write_attach_message(stream, AttachMessage::Unlock) {
        Err(ClientError::Io(error))
            if screen_tracker.was_stopped()
                && matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                ) =>
        {
            Ok(())
        }
        result => result,
    }
}

fn write_attach_data(
    stream: &mut UnixStream,
    bytes: &[u8],
) -> std::result::Result<(), ClientError> {
    if bytes.len() <= STACK_ATTACH_DATA_PAYLOAD {
        let mut frame = [0_u8; STACK_ATTACH_DATA_PAYLOAD + ATTACH_DATA_HEADER_LEN];
        let len = encode_attach_data_into_slice(bytes, &mut frame).map_err(ClientError::from)?;
        return stream.write_all(&frame[..len]).map_err(ClientError::Io);
    }

    let frame = encode_attach_data(bytes).map_err(ClientError::from)?;
    stream.write_all(&frame).map_err(ClientError::Io)
}

fn join_attach_thread(
    thread: thread::JoinHandle<std::result::Result<(), ClientError>>,
) -> std::result::Result<std::result::Result<(), ClientError>, ClientError> {
    thread
        .join()
        .map_err(|_| ClientError::Io(io::Error::other("attach thread panicked")))
}

fn shutdown_attach_writes(stream: &UnixStream) -> std::result::Result<(), ClientError> {
    match stream.shutdown(Shutdown::Write) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotConnected => Ok(()),
        Err(error) => Err(ClientError::Io(error)),
    }
}

#[derive(Debug)]
enum ClientAttachAction {
    Lock {
        command: String,
        stop_generation: Option<AttachStopGeneration>,
    },
    LockShell {
        command: AttachShellCommand,
        stop_generation: Option<AttachStopGeneration>,
    },
    Suspend {
        stop_generation: Option<AttachStopGeneration>,
    },
    DetachKill,
    DetachExec(String),
    DetachExecShell(AttachShellCommand),
}

#[derive(Debug)]
enum ClientAttachEvent {
    Action(ClientAttachAction),
    OutputDone,
}

#[cfg(test)]
mod tests;
