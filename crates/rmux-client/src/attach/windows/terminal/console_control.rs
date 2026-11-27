//! Console control (Ctrl-C/close/logoff) handling while raw mode is active.

use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, SetConsoleMode, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
};

use super::{
    vt_context, AttachError, ConsoleMode, RawTerminalGuard, Result, Win32Console, ATTACH_CONSOLE_IO,
};

#[derive(Debug)]
pub(super) struct ConsoleControlHandlerGuard;

impl ConsoleControlHandlerGuard {
    pub(super) fn install(terminal: &RawTerminalGuard<Win32Console>) -> Result<Self> {
        let snapshot = ConsoleModeSnapshot::from_terminal(terminal);
        let mut state = CTRL_HANDLER_STATE
            .lock()
            .expect("console control handler state poisoned");
        *state = Some(snapshot);

        let ok = unsafe {
            // SAFETY: `raw_terminal_ctrl_handler` is a process-static callback
            // with the Win32 signature required by SetConsoleCtrlHandler.
            SetConsoleCtrlHandler(Some(raw_terminal_ctrl_handler), 1)
        };
        if ok == 0 {
            *state = None;
            return Err(AttachError::Io(io::Error::last_os_error()));
        }
        CTRL_C_EVENT_ROUTER.enable();
        Ok(Self)
    }
}

impl Drop for ConsoleControlHandlerGuard {
    fn drop(&mut self) {
        CTRL_C_EVENT_ROUTER.disable();
        if let Ok(mut state) = CTRL_HANDLER_STATE.lock() {
            *state = None;
        }
        let _ = unsafe {
            // SAFETY: The callback was installed by this guard and the process
            // remains alive while the guard is being dropped.
            SetConsoleCtrlHandler(Some(raw_terminal_ctrl_handler), 0)
        };
    }
}

#[derive(Clone, Copy, Debug)]
struct ConsoleModeSnapshot {
    vt_input: bool,
    input: Option<ConsoleRestorePoint>,
    output: Option<ConsoleRestorePoint>,
}

impl ConsoleModeSnapshot {
    fn from_terminal(terminal: &RawTerminalGuard<Win32Console>) -> Self {
        Self {
            vt_input: terminal.vt_input,
            input: terminal.input.as_ref().map(ConsoleRestorePoint::from_mode),
            output: terminal.output.as_ref().map(ConsoleRestorePoint::from_mode),
        }
    }

    fn restore(self) {
        if let (true, Some(input), Some(output)) = (self.vt_input, self.input, self.output) {
            let _ = vt_context::set(
                input.handle as HANDLE,
                output.handle as HANDLE,
                input.mode,
                false,
            );
        }
        if let Some(input) = self.input {
            input.restore();
        }
        if let Some(output) = self.output {
            output.restore();
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ConsoleRestorePoint {
    handle: isize,
    mode: u32,
}

impl ConsoleRestorePoint {
    fn from_mode(mode: &ConsoleMode<HANDLE>) -> Self {
        Self {
            handle: mode.handle as isize,
            mode: mode.original,
        }
    }

    fn restore(self) {
        let _ = ATTACH_CONSOLE_IO.synchronized(|| unsafe {
            // SAFETY: The handle and mode were captured from a successful
            // GetConsoleMode call while entering raw mode. Restoration is best
            // effort because this may run from a console-control callback.
            SetConsoleMode(self.handle as HANDLE, self.mode)
        });
    }
}

static CTRL_HANDLER_STATE: Mutex<Option<ConsoleModeSnapshot>> = Mutex::new(None);
static CTRL_C_EVENT_ROUTER: CtrlCEventRouter = CtrlCEventRouter::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum CtrlCEventState {
    Disabled = 0,
    Active = 1,
    Pending = 2,
    Suppressed = 3,
}

#[derive(Debug)]
pub(super) struct CtrlCEventRouter {
    state: AtomicU8,
}

impl CtrlCEventRouter {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(CtrlCEventState::Disabled as u8),
        }
    }

    pub(super) fn enable(&self) {
        self.state
            .store(CtrlCEventState::Active as u8, Ordering::SeqCst);
    }

    fn disable(&self) {
        self.state
            .store(CtrlCEventState::Disabled as u8, Ordering::SeqCst);
    }

    pub(super) fn suppress(&self) {
        self.transition_to_suppressed();
    }

    pub(super) fn resume(&self) {
        let _ = self.state.compare_exchange(
            CtrlCEventState::Suppressed as u8,
            CtrlCEventState::Active as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub(super) fn record(&self) {
        loop {
            match self.state.load(Ordering::SeqCst) {
                state if state == CtrlCEventState::Active as u8 => {
                    if self
                        .state
                        .compare_exchange(
                            CtrlCEventState::Active as u8,
                            CtrlCEventState::Pending as u8,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    pub(super) fn take_pending(&self) -> bool {
        self.state
            .compare_exchange(
                CtrlCEventState::Pending as u8,
                CtrlCEventState::Active as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    fn transition_to_suppressed(&self) {
        loop {
            let current = self.state.load(Ordering::SeqCst);
            if matches!(
                current,
                state if state == CtrlCEventState::Disabled as u8
                    || state == CtrlCEventState::Suppressed as u8
            ) {
                return;
            }
            if self
                .state
                .compare_exchange(
                    current,
                    CtrlCEventState::Suppressed as u8,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return;
            }
        }
    }
}

unsafe extern "system" fn raw_terminal_ctrl_handler(event: u32) -> i32 {
    if event == CTRL_C_EVENT {
        CTRL_C_EVENT_ROUTER.record();
        return 1;
    }
    if should_restore_for_console_event(event) {
        if let Ok(state) = CTRL_HANDLER_STATE.lock() {
            if let Some(snapshot) = *state {
                snapshot.restore();
            }
        }
    }
    0
}

pub(in crate::attach::windows) fn take_pending_ctrl_c_event() -> bool {
    CTRL_C_EVENT_ROUTER.take_pending()
}

pub(in crate::attach::windows) fn suppress_ctrl_c_input() {
    CTRL_C_EVENT_ROUTER.suppress();
}

pub(in crate::attach::windows) fn resume_ctrl_c_input() {
    CTRL_C_EVENT_ROUTER.resume();
}

pub(super) const fn should_restore_for_console_event(event: u32) -> bool {
    matches!(
        event,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    )
}
