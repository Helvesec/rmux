//! Windows attach-mode client.

use std::io::{self, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;

use rmux_ipc::BlockingLocalStream;
use rmux_proto::{encode_attach_message, AttachMessage, TerminalSize};
use tokio::sync::mpsc;

use crate::ClientError;

#[path = "attach_windows/action.rs"]
mod action;
#[path = "attach_windows/input.rs"]
mod input;
#[path = "attach_windows/metrics.rs"]
mod metrics;
#[path = "attach/screen.rs"]
mod screen;
#[path = "attach_windows/stream.rs"]
mod stream;
#[path = "attach_windows/terminal.rs"]
mod terminal;
#[path = "attach/terminal_cleanup.rs"]
mod terminal_cleanup;

use screen::AttachScreenTracker;
pub use terminal::{AttachError, RawTerminal, Result};

const READ_BUFFER_SIZE: usize = 8192;

/// Runs the attach loop using the process stdin/stdout streams.
pub fn attach_terminal(stream: BlockingLocalStream) -> std::result::Result<(), ClientError> {
    attach_terminal_with_initial_bytes(stream, Vec::new())
}

/// Runs the attach loop using process stdin/stdout and pre-read stream bytes.
pub fn attach_terminal_with_initial_bytes(
    stream: BlockingLocalStream,
    initial_bytes: Vec<u8>,
) -> std::result::Result<(), ClientError> {
    let input = io::stdin();
    let output = io::stdout();

    attach_with_stdio(stream, initial_bytes, input, output)
}

/// Runs the attach loop with an explicit terminal handle.
///
/// Windows console mode is process-handle based, so `terminal` is accepted for
/// API parity with Unix but stdin/stdout are used to apply and restore modes.
pub fn attach_with_terminal<Terminal, Input, Output>(
    stream: BlockingLocalStream,
    _terminal: &Terminal,
    input: Input,
    output: Output,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsRawHandle + Send + 'static,
    Output: Write + Send + 'static,
{
    attach_with_stdio(stream, Vec::new(), input, output)
}

fn attach_with_stdio<Input, Output>(
    stream: BlockingLocalStream,
    initial_bytes: Vec<u8>,
    input: Input,
    output: Output,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsRawHandle + Send + 'static,
    Output: Write + Send + 'static,
{
    let raw_terminal = Arc::new(RawTerminal::enter().map_err(ClientError::from)?);
    let _ = raw_terminal.flush_pending_input();
    let screen_tracker = AttachScreenTracker::default();
    let result = drive_attach_stream_with_terminal_state(
        stream,
        initial_bytes,
        Arc::clone(&raw_terminal),
        &screen_tracker,
        input,
        output,
    );
    if result.is_err() && !screen_tracker.was_stopped() {
        let _ = raw_terminal.restore_attach_terminal_state();
    }
    let _ = raw_terminal.flush_pending_input();
    drop(raw_terminal);
    result
}

fn drive_attach_stream_with_terminal_state<Input, Output>(
    mut stream: BlockingLocalStream,
    initial_bytes: Vec<u8>,
    raw_terminal: Arc<RawTerminal>,
    screen_tracker: &AttachScreenTracker,
    input: Input,
    output: Output,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsRawHandle + Send + 'static,
    Output: Write + Send + 'static,
{
    let initial_size = terminal::current_size();
    if let Some(size) = initial_size {
        write_attach_message(&mut stream, AttachMessage::Resize(size))?;
    }
    let (resize_tx, resize_rx) = mpsc::unbounded_channel();
    let _resize_watcher = terminal::ResizeWatcher::spawn(initial_size, resize_tx);

    drive_attach_stream_inner(
        stream,
        initial_bytes,
        screen_tracker.clone(),
        input,
        output,
        resize_rx,
        action::ManagedTerminalActions::new(raw_terminal),
    )
}

/// Drives raw attach-stream byte forwarding over an upgraded local stream.
pub fn drive_attach_stream<Input, Output>(
    stream: BlockingLocalStream,
    input: Input,
    output: Output,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsRawHandle + Send + 'static,
    Output: Write + Send + 'static,
{
    drive_attach_stream_inner(
        stream,
        Vec::new(),
        AttachScreenTracker::default(),
        input,
        output,
        closed_resize_rx(),
        action::StreamOnlyActions,
    )
}

fn drive_attach_stream_inner<Input, Output, Actions>(
    stream: BlockingLocalStream,
    initial_bytes: Vec<u8>,
    screen_tracker: AttachScreenTracker,
    input: Input,
    output: Output,
    resize_rx: mpsc::UnboundedReceiver<TerminalSize>,
    actions: Actions,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsRawHandle + Send + 'static,
    Output: Write + Send + 'static,
    Actions: action::AttachActionExecutor + Send + 'static,
{
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let locked = Arc::new(AtomicBool::new(false));
    let input_locked = Arc::clone(&locked);
    let input_thread = thread::spawn(move || input_loop(input, input_tx, input_locked));
    let (action_tx, action_rx) = std_mpsc::channel();
    let (action_completion_tx, action_completion_rx) = mpsc::unbounded_channel();
    let action_thread =
        thread::spawn(move || action_loop(actions, action_rx, action_completion_tx));
    let (pipe, runtime) = stream.into_async_parts();
    let output_result = runtime.block_on(async {
        let (reader, writer) = tokio::io::split(pipe);
        stream::drive_async_attach(
            reader,
            writer,
            initial_bytes,
            output,
            screen_tracker,
            stream::AttachAsyncChannels::new(
                input_rx,
                resize_rx,
                action_tx,
                action_completion_rx,
                locked,
            ),
        )
        .await
    });

    let input_result = join_attach_thread(input_thread)?;
    let action_result = join_attach_thread(action_thread)?;

    output_result?;
    action_result?;
    input_result
}

fn action_loop<Actions>(
    mut actions: Actions,
    action_rx: std_mpsc::Receiver<action::AttachAction>,
    completion_tx: mpsc::UnboundedSender<
        std::result::Result<action::AttachActionOutcome, ClientError>,
    >,
) -> std::result::Result<(), ClientError>
where
    Actions: action::AttachActionExecutor,
{
    while let Ok(request) = action_rx.recv() {
        let result = action::run_attach_action(&mut actions, request);
        if completion_tx.send(result).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

fn input_loop<Input>(
    mut input: Input,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    locked: Arc<AtomicBool>,
) -> std::result::Result<(), ClientError>
where
    Input: Read + AsRawHandle,
{
    let mut read_buffer = [0_u8; READ_BUFFER_SIZE];
    let input_handle = input.as_raw_handle();

    loop {
        if locked.load(Ordering::SeqCst) {
            if input_tx.is_closed() {
                return Ok(());
            }
            thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }

        if !terminal::wait_for_key_input(input_handle, 50).map_err(ClientError::Io)? {
            if input_tx.is_closed() {
                return Ok(());
            }
            continue;
        }

        let bytes_read = match input.read(&mut read_buffer) {
            Ok(0) => return Ok(()),
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ClientError::Io(error)),
        };

        if input_tx.send(read_buffer[..bytes_read].to_vec()).is_err() {
            return Ok(());
        }
    }
}

fn write_attach_message(
    stream: &mut BlockingLocalStream,
    message: AttachMessage,
) -> std::result::Result<(), ClientError> {
    let frame = encode_attach_message(&message).map_err(ClientError::from)?;
    stream.write_all(&frame).map_err(ClientError::Io)
}

fn closed_resize_rx() -> mpsc::UnboundedReceiver<TerminalSize> {
    let (resize_tx, resize_rx) = mpsc::unbounded_channel();
    drop(resize_tx);
    resize_rx
}

fn join_attach_thread(
    thread: thread::JoinHandle<std::result::Result<(), ClientError>>,
) -> std::result::Result<std::result::Result<(), ClientError>, ClientError> {
    thread
        .join()
        .map_err(|_| ClientError::Io(io::Error::other("attach thread panicked")))
}
