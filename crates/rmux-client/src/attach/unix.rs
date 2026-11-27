//! Raw terminal lifecycle and attach-stream helpers for attach-mode clients.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use rmux_proto::{
    encode_attach_data, encode_attach_data_into_slice, encode_attach_message, AttachMessage,
    TerminalGeometry, TerminalSize, ATTACH_DATA_HEADER_LEN,
};
use rustix::event::{poll, PollFd, PollFlags, Timespec};

use crate::attach_lock_state::AttachLockState;
use crate::ClientError;

use super::{screen, terminal_cleanup};

mod actions;
mod output;
mod render_drain;
mod resize;
mod terminal;
mod termination;

#[cfg(test)]
use actions::{handle_attach_action, write_attach_unlock, ClientAttachAction};
use actions::{wait_for_output_thread, ClientAttachEvent};
#[cfg(test)]
use output::output_loop;
use output::output_loop_with_termination;
#[cfg(test)]
use resize::terminal_size_from_fd;
use resize::{terminal_geometry_from_fd, ResizeWatcher, SignalMaskGuard};
use screen::AttachScreenTracker;
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
