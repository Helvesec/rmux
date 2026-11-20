use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Write};
use std::process::Stdio;
use std::sync::Mutex;

use rmux_core::{alternate_screen_enter_sequence, alternate_screen_exit_sequence};
use rmux_os::process_tree::ProcessTreeChild;
use rmux_proto::AttachShellCommand;
#[cfg(test)]
use rmux_proto::TerminalSize;
use windows_sys::Win32::Foundation::{ERROR_INVALID_HANDLE, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Console::{
    GetNumberOfConsoleInputEvents, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT,
    ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE,
};
#[cfg(test)]
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

use super::console_coordination::ATTACH_CONSOLE_IO;
mod vt_context;
use super::shell_command::{command_from_legacy, command_from_spec};

mod console_control;
mod resize;
use console_control::ConsoleControlHandlerGuard;
pub(super) use console_control::{
    resume_ctrl_c_input, suppress_ctrl_c_input, take_pending_ctrl_c_event,
};
#[cfg(test)]
use console_control::{should_restore_for_console_event, CtrlCEventRouter};
#[cfg(test)]
use resize::ResizeDeduper;
pub(super) use resize::{current_size, ResizeWatcher};

pub(super) fn vt_input_context_active(handle: HANDLE) -> bool {
    vt_context::is_active(handle)
}

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
    _ctrl_handler: ConsoleControlHandlerGuard,
}

// SAFETY: RawTerminal stores process-wide Win32 console HANDLE values and
// immutable mode snapshots only. The attach lifecycle serializes semantic
// ownership by moving the guard into the attach action worker.
unsafe impl Send for RawTerminal {}

impl RawTerminal {
    /// Enters raw mode for process stdin/stdout console handles.
    pub fn enter() -> Result<Self> {
        let inner = RawTerminalGuard::enter(Win32Console)?;
        let ctrl_handler = ConsoleControlHandlerGuard::install(&inner)?;
        Ok(Self {
            inner,
            _ctrl_handler: ctrl_handler,
        })
    }

    /// Restores the terminal settings captured when the guard was created.
    pub fn restore(&self) -> Result<()> {
        self.inner.restore()
    }

    pub(super) fn run_lock_command(&self, command: &AttachShellCommand) -> Result<()> {
        self.restore()?;
        let command_result = run_shell_command(command_from_spec(command));
        let raw_result = self
            .inner
            .flush_pending_input()
            .and_then(|()| self.inner.reapply_raw_mode());
        if let Err(error) = command_result {
            raw_result?;
            return Err(error);
        }
        raw_result?;
        Ok(())
    }

    pub(super) fn set_mouse_input_enabled(&self, enabled: bool) -> Result<()> {
        self.inner.set_mouse_input_enabled(enabled)
    }

    pub(super) fn run_legacy_lock_command(&self, command: &str) -> Result<()> {
        self.restore()?;
        let command_result = run_shell_command(command_from_legacy(command));
        let raw_result = self
            .inner
            .flush_pending_input()
            .and_then(|()| self.inner.reapply_raw_mode());
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

    pub(super) fn run_detach_exec_command(&self, command: &AttachShellCommand) -> Result<()> {
        self.restore()?;
        run_shell_command(command_from_spec(command))
    }

    pub(super) fn run_legacy_detach_exec_command(&self, command: &str) -> Result<()> {
        self.restore()?;
        run_shell_command(command_from_legacy(command))
    }

    pub(super) fn flush_pending_input(&self) -> Result<()> {
        self.inner.flush_pending_input()
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = self.flush_pending_input();
    }
}

type ConsoleModePair<'a, Handle> = (&'a ConsoleMode<Handle>, &'a ConsoleMode<Handle>);

#[derive(Debug)]
struct RawTerminalGuard<C: ConsoleApi> {
    console: C,
    input: Option<ConsoleMode<C::Handle>>,
    output: Option<ConsoleMode<C::Handle>>,
    mouse_enabled: Mutex<bool>,
    vt_input: bool,
}

impl<C: ConsoleApi> RawTerminalGuard<C> {
    fn enter(console: C) -> Result<Self> {
        let input = ConsoleMode::for_std_handle(&console, STD_INPUT_HANDLE)?;
        let output = ConsoleMode::for_std_handle(&console, STD_OUTPUT_HANDLE)?;
        let vt_input = input.is_some() && output.is_some() && console.supports_vt_input();
        let guard = Self {
            console,
            input,
            output,
            mouse_enabled: Mutex::new(false),
            vt_input,
        };

        if let Some(input) = &guard.input {
            input.set(&guard.console, raw_input_mode(input.original))?;
        }
        if let Some(output) = &guard.output {
            output.set(&guard.console, raw_output_mode(output.original))?;
        }

        guard.apply_vt_input(false)?;
        Ok(guard)
    }

    fn restore(&self) -> Result<()> {
        if let Some((input, output)) = self.vt_modes() {
            let result =
                self.console
                    .set_vt_input(input.handle, output.handle, input.original, false);
            return result.and(output.restore(&self.console));
        }
        if let Some(input) = &self.input {
            input.restore(&self.console)?;
        }
        if let Some(output) = &self.output {
            output.restore(&self.console)?;
        }
        Ok(())
    }

    fn reapply_raw_mode(&self) -> Result<()> {
        if let Some((_, output)) = self.vt_modes() {
            output.set(&self.console, raw_output_mode(output.original))?;
            return self.apply_vt_input(self.mouse_input_enabled());
        }
        if let Some(input) = &self.input {
            input.set(
                &self.console,
                raw_input_mode_with_mouse(input.original, self.mouse_input_enabled()),
            )?;
        }
        if let Some(output) = &self.output {
            output.set(&self.console, raw_output_mode(output.original))?;
        }
        Ok(())
    }

    fn set_mouse_input_enabled(&self, enabled: bool) -> Result<()> {
        if self.vt_input {
            self.apply_vt_input(enabled)?;
        } else if let Some(input) = &self.input {
            input.set(
                &self.console,
                raw_input_mode_with_mouse(input.original, enabled),
            )?;
        }
        *self
            .mouse_enabled
            .lock()
            .map_err(|_| AttachError::Io(io::Error::other("mouse mode mutex poisoned")))? = enabled;
        Ok(())
    }

    fn vt_modes(&self) -> Option<ConsoleModePair<'_, C::Handle>> {
        if self.vt_input {
            self.input.as_ref().zip(self.output.as_ref())
        } else {
            None
        }
    }

    fn apply_vt_input(&self, mouse: bool) -> Result<()> {
        if let Some((input, output)) = self.vt_modes() {
            self.console.set_vt_input(
                input.handle,
                output.handle,
                raw_input_mode_with_mouse(input.original, mouse),
                true,
            )?;
        }
        Ok(())
    }

    fn mouse_input_enabled(&self) -> bool {
        self.mouse_enabled
            .lock()
            .map(|enabled| *enabled)
            .unwrap_or(false)
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

    fn supports_vt_input(&self) -> bool {
        false
    }
    fn set_vt_input(
        &self,
        input: Self::Handle,
        _output: Self::Handle,
        mode: u32,
        _enabled: bool,
    ) -> Result<()> {
        self.set_console_mode(input, mode)
    }
    fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>>;
    fn get_console_mode(&self, handle: Self::Handle) -> Result<Option<u32>>;
    fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<()>;
    fn flush_console_input(&self, handle: Self::Handle) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct Win32Console;

mod console_api;

pub(super) fn wait_for_key_input(handle: HANDLE, timeout_ms: u32) -> io::Result<bool> {
    match unsafe {
        // SAFETY: handle is borrowed only for the duration of this wait.
        WaitForSingleObject(handle, timeout_ms)
    } {
        WAIT_OBJECT_0 => console_input_is_readable(handle),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

fn console_input_is_readable(handle: HANDLE) -> io::Result<bool> {
    ATTACH_CONSOLE_IO.synchronized(|| {
        let mut event_count = 0;
        let ok = unsafe {
            // SAFETY: handle is borrowed and event_count points to writable storage.
            GetNumberOfConsoleInputEvents(handle, &mut event_count)
        };
        if ok == 0 {
            return invalid_console_input_as_readable();
        }
        Ok(event_count > 0)
    })?
}

fn invalid_console_input_as_readable() -> io::Result<bool> {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_INVALID_HANDLE as i32) {
        return Ok(true);
    }
    Err(error)
}

const fn raw_input_mode(original: u32) -> u32 {
    raw_input_mode_with_mouse(original, false)
}

const fn raw_input_mode_with_mouse(original: u32, mouse_enabled: bool) -> u32 {
    let line_editing = ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT;
    if mouse_enabled {
        // Mouse reporting must not be intercepted by console text selection.
        (original | ENABLE_EXTENDED_FLAGS | ENABLE_MOUSE_INPUT)
            & !(line_editing | ENABLE_QUICK_EDIT_MODE)
    } else {
        (original | ENABLE_EXTENDED_FLAGS)
            & !(line_editing | ENABLE_MOUSE_INPUT | ENABLE_QUICK_EDIT_MODE)
    }
}

const fn raw_output_mode(original: u32) -> u32 {
    original | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN
}

fn run_shell_command(mut child: std::process::Command) -> Result<()> {
    let mut stdout = io::stdout();
    let term = std::env::var("TERM").unwrap_or_default();

    stdout.write_all(alternate_screen_enter_sequence(&term))?;
    stdout.flush()?;

    child
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status_result = ProcessTreeChild::spawn(&mut child).and_then(|mut child| child.wait());

    stdout.write_all(alternate_screen_exit_sequence(&term))?;
    stdout.flush()?;
    status_result.map_err(AttachError::Io)?;
    Ok(())
}

#[cfg(test)]
#[path = "../../attach_windows/terminal_tests.rs"]
mod tests;
