use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use super::*;

const INPUT_HANDLE: u8 = 1;
const OUTPUT_HANDLE: u8 = 2;
const PRESERVED_INPUT_FLAG: u32 = 0x0100_0000;
const PRESERVED_OUTPUT_FLAG: u32 = 0x0200_0000;

#[test]
fn enter_applies_raw_input_and_vt_output_flags() -> Result<()> {
    let input_original =
        ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT | PRESERVED_INPUT_FLAG;
    let output_original = PRESERVED_OUTPUT_FLAG;
    let console = FakeConsole::new(Some(input_original), Some(output_original));

    let _guard = RawTerminalGuard::enter(console.clone())?;

    assert_eq!(
        console.mode(INPUT_HANDLE),
        Some(PRESERVED_INPUT_FLAG | ENABLE_VIRTUAL_TERMINAL_INPUT)
    );
    assert_eq!(
        console.mode(OUTPUT_HANDLE),
        Some(PRESERVED_OUTPUT_FLAG | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
    );
    assert_eq!(
        console.set_calls(),
        vec![
            (
                INPUT_HANDLE,
                PRESERVED_INPUT_FLAG | ENABLE_VIRTUAL_TERMINAL_INPUT
            ),
            (
                OUTPUT_HANDLE,
                PRESERVED_OUTPUT_FLAG | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            ),
        ]
    );
    Ok(())
}

#[test]
fn explicit_restore_and_drop_restore_original_modes() -> Result<()> {
    let input_original = ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT;
    let output_original = PRESERVED_OUTPUT_FLAG;
    let console = FakeConsole::new(Some(input_original), Some(output_original));

    {
        let guard = RawTerminalGuard::enter(console.clone())?;
        guard.restore()?;
        guard.restore()?;
        assert_eq!(console.mode(INPUT_HANDLE), Some(input_original));
        assert_eq!(console.mode(OUTPUT_HANDLE), Some(output_original));
    }

    assert_eq!(console.mode(INPUT_HANDLE), Some(input_original));
    assert_eq!(console.mode(OUTPUT_HANDLE), Some(output_original));
    Ok(())
}

#[test]
fn reapply_raw_mode_restores_raw_flags_after_explicit_restore() -> Result<()> {
    let input_original =
        ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT | PRESERVED_INPUT_FLAG;
    let output_original = PRESERVED_OUTPUT_FLAG;
    let console = FakeConsole::new(Some(input_original), Some(output_original));
    let guard = RawTerminalGuard::enter(console.clone())?;

    guard.restore()?;
    guard.reapply_raw_mode()?;

    assert_eq!(
        console.mode(INPUT_HANDLE),
        Some(PRESERVED_INPUT_FLAG | ENABLE_VIRTUAL_TERMINAL_INPUT)
    );
    assert_eq!(
        console.mode(OUTPUT_HANDLE),
        Some(PRESERVED_OUTPUT_FLAG | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
    );
    Ok(())
}

#[test]
fn drop_restores_original_modes_after_panic() {
    let input_original = ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT;
    let output_original = PRESERVED_OUTPUT_FLAG;
    let console = FakeConsole::new(Some(input_original), Some(output_original));

    let panic_result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = RawTerminalGuard::enter(console.clone()).expect("enter raw mode");
        panic!("intentional panic while raw mode is active");
    }));

    assert!(panic_result.is_err());
    assert_eq!(console.mode(INPUT_HANDLE), Some(input_original));
    assert_eq!(console.mode(OUTPUT_HANDLE), Some(output_original));
}

#[test]
fn enter_failure_rolls_back_already_changed_modes() {
    let input_original = ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT;
    let output_original = PRESERVED_OUTPUT_FLAG;
    let console = FakeConsole::new(Some(input_original), Some(output_original));
    console.fail_next_set_for(OUTPUT_HANDLE);

    let error = RawTerminalGuard::enter(console.clone()).expect_err("output set should fail");

    assert!(matches!(error, AttachError::Io(_)));
    assert_eq!(console.mode(INPUT_HANDLE), Some(input_original));
    assert_eq!(console.mode(OUTPUT_HANDLE), Some(output_original));
}

#[test]
fn flush_pending_input_uses_input_handle_when_present() -> Result<()> {
    let console = FakeConsole::new(Some(ENABLE_LINE_INPUT), None);
    let guard = RawTerminalGuard::enter(console.clone())?;

    guard.flush_pending_input()?;

    assert_eq!(console.flushed_handles(), vec![INPUT_HANDLE]);
    Ok(())
}

#[test]
fn flush_pending_input_ignores_redirected_input() -> Result<()> {
    let console = FakeConsole::new(None, Some(PRESERVED_OUTPUT_FLAG));
    let guard = RawTerminalGuard::enter(console.clone())?;

    guard.flush_pending_input()?;

    assert!(console.flushed_handles().is_empty());
    Ok(())
}

#[test]
fn resize_deduper_reports_only_real_size_changes() {
    let initial = Some(TerminalSize { cols: 80, rows: 24 });
    let mut deduper = ResizeDeduper::new(initial);

    assert_eq!(deduper.observe(initial), None);
    assert_eq!(deduper.observe(None), None);
    assert_eq!(
        deduper.observe(Some(TerminalSize {
            cols: 100,
            rows: 30
        })),
        Some(TerminalSize {
            cols: 100,
            rows: 30
        })
    );
    assert_eq!(
        deduper.observe(Some(TerminalSize {
            cols: 100,
            rows: 30
        })),
        None
    );
}

#[test]
fn windows_command_shell_preserves_command_payload_as_one_arg() {
    let command = "echo lock command && exit /b 0";
    let child = windows_command_shell(command);
    let args = child
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(args, vec!["/D", "/C", command]);
}

#[derive(Clone, Debug)]
struct FakeConsole {
    state: Rc<RefCell<FakeConsoleState>>,
}

impl FakeConsole {
    fn new(input_mode: Option<u32>, output_mode: Option<u32>) -> Self {
        let mut std_handles = BTreeMap::new();
        let mut modes = BTreeMap::new();
        if let Some(mode) = input_mode {
            std_handles.insert(STD_INPUT_HANDLE, INPUT_HANDLE);
            modes.insert(INPUT_HANDLE, mode);
        }
        if let Some(mode) = output_mode {
            std_handles.insert(STD_OUTPUT_HANDLE, OUTPUT_HANDLE);
            modes.insert(OUTPUT_HANDLE, mode);
        }

        Self {
            state: Rc::new(RefCell::new(FakeConsoleState {
                std_handles,
                modes,
                set_calls: Vec::new(),
                flushed_handles: Vec::new(),
                failing_set_handles: VecDeque::new(),
            })),
        }
    }

    fn mode(&self, handle: u8) -> Option<u32> {
        self.state.borrow().modes.get(&handle).copied()
    }

    fn set_calls(&self) -> Vec<(u8, u32)> {
        self.state.borrow().set_calls.clone()
    }

    fn flushed_handles(&self) -> Vec<u8> {
        self.state.borrow().flushed_handles.clone()
    }

    fn fail_next_set_for(&self, handle: u8) {
        self.state
            .borrow_mut()
            .failing_set_handles
            .push_back(handle);
    }
}

#[derive(Debug)]
struct FakeConsoleState {
    std_handles: BTreeMap<u32, u8>,
    modes: BTreeMap<u8, u32>,
    set_calls: Vec<(u8, u32)>,
    flushed_handles: Vec<u8>,
    failing_set_handles: VecDeque<u8>,
}

impl ConsoleApi for FakeConsole {
    type Handle = u8;

    fn std_handle(&self, handle_id: u32) -> Result<Option<Self::Handle>> {
        Ok(self.state.borrow().std_handles.get(&handle_id).copied())
    }

    fn get_console_mode(&self, handle: Self::Handle) -> Result<Option<u32>> {
        Ok(self.state.borrow().modes.get(&handle).copied())
    }

    fn set_console_mode(&self, handle: Self::Handle, mode: u32) -> Result<()> {
        let mut state = self.state.borrow_mut();
        if state.failing_set_handles.front() == Some(&handle) {
            state.failing_set_handles.pop_front();
            return Err(AttachError::Io(io::Error::other(
                "injected console mode failure",
            )));
        }
        state.modes.insert(handle, mode);
        state.set_calls.push((handle, mode));
        Ok(())
    }

    fn flush_console_input(&self, handle: Self::Handle) -> Result<()> {
        self.state.borrow_mut().flushed_handles.push(handle);
        Ok(())
    }
}
