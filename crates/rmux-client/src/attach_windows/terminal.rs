use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rmux_core::{alternate_screen_enter_sequence, alternate_screen_exit_sequence};
use rmux_proto::TerminalSize;
use tokio::sync::mpsc;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_INVALID_HANDLE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{
    FlushConsoleInputBuffer, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
    PeekConsoleInputW, ReadConsoleInputW, SetConsoleMode, CONSOLE_SCREEN_BUFFER_INFO,
    ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, INPUT_RECORD, KEY_EVENT, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

use super::terminal_cleanup::fallback_attach_stop_sequence;

/// Result type for raw-terminal lifecycle operations.
pub type Result<T> = std::result::Result<T, AttachError>;

/// Errors produced while entering or restoring raw terminal mode.
#[derive(Debug)]
pub enum AttachError {
    /// A Win32 console operation failed.
    Io(io::Error),
}

impl fmt::Display for AttachError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "terminal console operation failed: {error}"),
        }
    }
}

impl StdError for AttachError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for AttachError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A drop guard that applies Windows console raw-ish VT mode and restores the
/// original settings when dropped.
#[derive(Debug)]
#[must_use = "keep the guard alive for as long as raw terminal mode is required"]
pub struct RawTerminal {
    inner: RawTerminalGuard<Win32Console>,
}

// SAFETY: RawTerminal stores process-wide Win32 console HANDLE values and
// immutable mode snapshots only. The attach lifecycle serializes semantic
// ownership by joining the action worker before dropping the guard.
unsafe impl Send for RawTerminal {}
// SAFETY: RawTerminal methods operate through Win32 console APIs using shared
// references; there is no Rust-managed mutable state behind those references.
unsafe impl Sync for RawTerminal {}

impl RawTerminal {
    /// Enters raw mode for process stdin/stdout console handles.
    pub fn enter() -> Result<Self> {
        Ok(Self {
            inner: RawTerminalGuard::enter(Win32Console)?,
        })
    }

    /// Restores the terminal settings captured when the guard was created.
    pub fn restore(&self) -> Result<()> {
        self.inner.restore()
    }

    pub(super) fn run_lock_command(&self, command: &str) -> Result<()> {
        self.restore()?;
        let command_result = run_shell_command(command);
        let raw_result = self.inner.reapply_raw_mode();
        if let Err(error) = command_result {
            raw_result?;
            return Err(error);
        }
        raw_result?;
        Ok(())
    }

    pub(super) fn suspend_self(&self) -> Result<()> {
        self.restore()?;
        // Windows has no SIGTSTP/job-control equivalent for console clients.
        // Re-enter raw mode immediately so the server observes the same
        // lock/unlock lifecycle without inventing extra tmux behavior.
        self.inner.reapply_raw_mode()
    }

    pub(super) fn run_detach_exec_command(&self, command: &str) -> Result<()> {
        self.restore()?;
        run_shell_command(command)
    }

    pub(super) fn restore_attach_terminal_state(&self) -> Result<()> {
        let mut stdout = io::stdout();
        let term = std::env::var("TERM").unwrap_or_default();
        stdout.write_all(&fallback_attach_stop_sequence(&term))?;
        stdout.flush()?;
        Ok(())
    }

    pub(super) fn flush_pending_input(&self) -> Result<()> {
        self.inner.flush_pending_input()
    }
}

#[derive(Debug)]
struct RawTerminalGuard<C: ConsoleApi> {
    console: C,
    input: Option<ConsoleMode<C::Handle>>,
    output: Option<ConsoleMode<C::Handle>>,
}

impl<C: ConsoleApi> RawTerminalGuard<C> {
    fn enter(console: C) -> Result<Self> {
        let input = ConsoleMode::for_std_handle(&console, STD_INPUT_HANDLE)?;
        let output = ConsoleMode::for_std_handle(&console, STD_OUTPUT_HANDLE)?;
        let guard = Self {
            console,
            input,
            output,
        };

        if let Some(input) = &guard.input {
            input.set(&guard.console, raw_input_mode(input.original))?;
        }
        if let Some(output) = &guard.output {
            output.set(&guard.console, raw_output_mode(output.original))?;
        }

        Ok(guard)
    }

    fn restore(&self) -> Result<()> {
        if let Some(input) = &self.input {
            input.restore(&self.console)?;
        }
        if let Some(output) = &self.output {
            output.restore(&self.console)?;
        }
        Ok(())
    }

    fn reapply_raw_mode(&self) -> Result<()> {
        if let Some(input) = &self.input {
            input.set(&self.console, raw_input_mode(input.original))?;
        }
        if let Some(output) = &self.output {
            output.set(&self.console, raw_output_mode(output.original))?;
        }
        Ok(())
    }

    fn flush_pending_input(&self) -> Result<()> {
        let Some(input) = &self.input else {
            return Ok(());
        };
        self.console.flush_console_input(input.handle)
    }
}

impl<C: ConsoleApi> Drop for RawTerminalGuard<C> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Debug)]
struct ConsoleMode<Handle> {
    handle: Handle,
    original: u32,
}

impl<Handle: Copy> ConsoleMode<Handle> {
    fn for_std_handle<C>(console: &C, handle_id: u32) -> Result<Option<Self>>
    where
        C: ConsoleApi<Handle = Handle>,
    {
        let handle = console.std_handle(handle_id)?;
        let Some(handle) = handle else {
            return Ok(None);
        };

        let Some(mode) = console.get_console_mode(handle)? else {
            return Ok(None);
        };

        Ok(Some(Self {
            handle,
            original: mode,
        }))
    }

    fn set<C>(&self, console: &C, mode: u32) -> Result<()>
    where
        C: ConsoleApi<Handle = Handle>,
    {
        console.set_console_mode(self.handle, mode)
    }

    fn restore<C>(&self, console: &C) -> Result<()>
    where
        C: ConsoleApi<Handle = Handle>,
    {
        self.set(console, self.original)
    }
}

trait ConsoleApi: std::fmt::Debug {
    type Handle: Copy + std::fmt::Debug;

    fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>>;
    fn get_console_mode(&self, handle: Self::Handle) -> Result<Option<u32>>;
    fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<()>;
    fn flush_console_input(&self, handle: Self::Handle) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct Win32Console;

impl ConsoleApi for Win32Console {
    type Handle = HANDLE;

    fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>> {
        std_handle(handle_id)
    }

    fn get_console_mode(&self, handle: Self::Handle) -> Result<Option<u32>> {
        let mut mode = 0;
        let ok = unsafe {
            // SAFETY: handle is a valid std handle and mode points to writable storage.
            GetConsoleMode(handle, &mut mode)
        };
        if ok == 0 {
            return console_mode_absent_or_error();
        }
        Ok(Some(mode))
    }

    fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<()> {
        let ok = unsafe {
            // SAFETY: handle is a console handle and mode is a bitmask accepted by Win32.
            SetConsoleMode(handle, mode)
        };
        if ok == 0 {
            return Err(AttachError::Io(io::Error::last_os_error()));
        }
        Ok(())
    }

    fn flush_console_input(&self, handle: Self::Handle) -> Result<()> {
        let ok = unsafe {
            // SAFETY: handle is a valid console input handle captured by ConsoleMode.
            FlushConsoleInputBuffer(handle)
        };
        if ok == 0 {
            return Err(AttachError::Io(io::Error::last_os_error()));
        }
        Ok(())
    }
}

pub(super) fn current_size() -> Option<TerminalSize> {
    let handle = std_handle(STD_OUTPUT_HANDLE).ok().flatten()?;
    let mut info = std::mem::MaybeUninit::<CONSOLE_SCREEN_BUFFER_INFO>::zeroed();
    let ok = unsafe {
        // SAFETY: info is writable for the Win32 structure expected by this API.
        GetConsoleScreenBufferInfo(handle, info.as_mut_ptr())
    };
    if ok == 0 {
        return None;
    }

    let info = unsafe {
        // SAFETY: Win32 reported that it initialized the structure.
        info.assume_init()
    };
    let width = info.srWindow.Right - info.srWindow.Left + 1;
    let height = info.srWindow.Bottom - info.srWindow.Top + 1;
    let cols = u16::try_from(width).ok()?;
    let rows = u16::try_from(height).ok()?;
    (cols > 0 && rows > 0).then_some(TerminalSize { cols, rows })
}

#[derive(Debug)]
pub(super) struct ResizeWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    pub(super) fn spawn(
        initial_size: Option<TerminalSize>,
        resize_tx: mpsc::UnboundedSender<TerminalSize>,
    ) -> Option<Self> {
        let initial_size = initial_size?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut deduper = ResizeDeduper::new(Some(initial_size));
            while !thread_stop.load(Ordering::SeqCst) && !resize_tx.is_closed() {
                thread::sleep(Duration::from_millis(100));
                if let Some(size) = deduper.observe(current_size()) {
                    if resize_tx.send(size).is_err() {
                        return;
                    }
                }
            }
        });

        Some(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug)]
struct ResizeDeduper {
    last: Option<TerminalSize>,
}

impl ResizeDeduper {
    const fn new(initial: Option<TerminalSize>) -> Self {
        Self { last: initial }
    }

    fn observe(&mut self, size: Option<TerminalSize>) -> Option<TerminalSize> {
        if size.is_some() && size != self.last {
            self.last = size;
            return size;
        }
        None
    }
}

pub(super) fn wait_for_key_input(handle: HANDLE, timeout_ms: u32) -> io::Result<bool> {
    match unsafe {
        // SAFETY: handle is borrowed only for the duration of this wait.
        WaitForSingleObject(handle, timeout_ms)
    } {
        WAIT_OBJECT_0 => discard_non_key_console_events(handle),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

fn discard_non_key_console_events(handle: HANDLE) -> io::Result<bool> {
    loop {
        let mut record = MaybeUninit::<INPUT_RECORD>::zeroed();
        let mut read = 0;
        let ok = unsafe {
            // SAFETY: record points to writable storage for one console input record.
            PeekConsoleInputW(handle, record.as_mut_ptr(), 1, &mut read)
        };
        if ok == 0 {
            return invalid_console_input_as_readable();
        }
        if read == 0 {
            return Ok(false);
        }

        let mut record = unsafe {
            // SAFETY: PeekConsoleInputW initialized one record when read is non-zero.
            record.assume_init()
        };
        if is_key_down_event(&record) {
            return Ok(true);
        }

        let ok = unsafe {
            // SAFETY: record is initialized and the call consumes exactly one non-key event.
            ReadConsoleInputW(handle, &mut record, 1, &mut read)
        };
        if ok == 0 {
            return invalid_console_input_as_readable();
        }
    }
}

fn is_key_down_event(record: &INPUT_RECORD) -> bool {
    if u32::from(record.EventType) != KEY_EVENT {
        return false;
    }
    let key = unsafe {
        // SAFETY: EventType is KEY_EVENT, so the KeyEvent union field is active.
        record.Event.KeyEvent
    };
    key.bKeyDown != 0
}

fn invalid_console_input_as_readable() -> io::Result<bool> {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_INVALID_HANDLE as i32) {
        return Ok(true);
    }
    Err(error)
}

fn std_handle(handle_id: u32) -> Result<Option<HANDLE>> {
    let handle = unsafe {
        // SAFETY: GetStdHandle accepts the documented STD_* constants.
        GetStdHandle(handle_id)
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Ok(None);
    }
    Ok(Some(handle))
}

fn console_mode_absent_or_error<T>() -> Result<Option<T>> {
    let error = unsafe {
        // SAFETY: GetLastError reads the calling thread's last Win32 error.
        GetLastError()
    };
    if error == ERROR_INVALID_HANDLE {
        return Ok(None);
    }
    Err(AttachError::Io(io::Error::from_raw_os_error(
        i32::try_from(error).unwrap_or(i32::MAX),
    )))
}

const fn raw_input_mode(original: u32) -> u32 {
    (original | ENABLE_VIRTUAL_TERMINAL_INPUT)
        & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT)
}

const fn raw_output_mode(original: u32) -> u32 {
    original | ENABLE_VIRTUAL_TERMINAL_PROCESSING
}

fn run_shell_command(command: &str) -> Result<()> {
    let mut stdout = io::stdout();
    let term = std::env::var("TERM").unwrap_or_default();

    stdout.write_all(alternate_screen_enter_sequence(&term))?;
    stdout.flush()?;

    let status_result = windows_command_shell(command)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    stdout.write_all(alternate_screen_exit_sequence(&term))?;
    stdout.flush()?;
    status_result.map_err(AttachError::Io)?;
    Ok(())
}

fn windows_command_shell(command: &str) -> Command {
    let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
    let mut child = Command::new(shell);
    child.arg("/D").arg("/C").arg(command);
    child
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
