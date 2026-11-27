use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Command;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    AllocConsole, FreeConsole, GetConsoleMode, GetNumberOfConsoleInputEvents, ReadConsoleInputW,
    SetConsoleMode, SetStdHandle, WriteConsoleW, DISABLE_NEWLINE_AUTO_RETURN,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, INPUT_RECORD, KEY_EVENT,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;

use super::{output::AttachStdout, RawTerminal};

#[path = "console_mode_child_selection.rs"]
mod child_selection;
#[path = "console_mode_decrqm_terminal.rs"]
mod decrqm_terminal;
use decrqm_terminal::ReportingOracle;

const CONSOLE_MODE_CHILD_ENV: &str = "RMUX_INTERNAL_CONSOLE_MODE_OWNER_TEST_CHILD";

fn console_mode_test_name() -> String {
    child_selection::exact_test_name(
        module_path!(),
        "managed_attach_stdout_has_single_console_mode_owner",
    )
}

#[test]
fn console_child_filter_tracks_module() {
    let (_, module) = module_path!().split_once("::").expect("test module path");
    assert_eq!(
        console_mode_test_name(),
        format!("{module}::managed_attach_stdout_has_single_console_mode_owner")
    );
}

#[test]
fn managed_attach_stdout_has_single_console_mode_owner() {
    if std::env::var_os(CONSOLE_MODE_CHILD_ENV).is_none() {
        let receipt = child_selection::ChildReceipt::new();
        let status = Command::new(
            rmux_os::build_identity::resolve(
                std::env::current_exe()
                    .expect("current test executable")
                    .as_os_str(),
            )
            .expect("verify console-mode test child"),
        )
        .args(["--exact", &console_mode_test_name(), "--nocapture"])
        .env(CONSOLE_MODE_CHILD_ENV, "1")
        .env(child_selection::RECEIPT_ENV, receipt.path())
        .status()
        .expect("spawn isolated console-mode test child");
        assert!(status.success(), "console-mode test child failed: {status}");
        receipt
            .verify(status.success(), &console_mode_test_name())
            .expect("console child must complete its exact test once");
        return;
    }

    unsafe {
        // SAFETY: This branch runs in a dedicated child process. Replacing its
        // process-wide console cannot affect the parent test harness.
        let _ = FreeConsole();
        assert_ne!(
            AllocConsole(),
            0,
            "AllocConsole failed: {}",
            io::Error::last_os_error()
        );
    }
    let conout = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .expect("open isolated console output");
    let output_handle = conout.as_raw_handle() as HANDLE;
    let set_stdout = unsafe {
        // SAFETY: output_handle remains owned by conout for the rest of this
        // child process and is a validated CONOUT$ handle.
        SetStdHandle(STD_OUTPUT_HANDLE, output_handle)
    };
    assert_ne!(
        set_stdout,
        0,
        "SetStdHandle failed: {}",
        io::Error::last_os_error()
    );

    let original = read_console_mode(output_handle);
    let initial = original & !ENABLE_VIRTUAL_TERMINAL_PROCESSING & !DISABLE_NEWLINE_AUTO_RETURN;
    write_console_mode(output_handle, initial);

    let terminal = RawTerminal::enter().expect("enter managed raw terminal");
    let stdout = AttachStdout::for_managed_terminal(io::sink());
    drop(stdout);
    drop(terminal);
    assert_eq!(
        read_console_mode(output_handle),
        initial,
        "writer-first teardown must restore the caller's exact output mode"
    );

    let terminal = RawTerminal::enter().expect("re-enter managed raw terminal");
    let stdout = AttachStdout::for_managed_terminal(io::sink());
    drop(terminal);
    drop(stdout);
    assert_eq!(
        read_console_mode(output_handle),
        initial,
        "terminal-first teardown must not let the writer restore a stale raw mode"
    );

    exercise_redirected_stdout(output_handle, initial);

    let _ = unsafe {
        // SAFETY: output_handle remains live until conout is dropped.
        SetStdHandle(STD_OUTPUT_HANDLE, output_handle)
    };
    write_console_mode(output_handle, original);
    child_selection::record_completion(&console_mode_test_name());
}

fn vt_restore_test_name() -> String {
    child_selection::exact_test_name(
        module_path!(),
        "vt_raw_terminal_restores_both_console_modes",
    )
}

#[test]
fn vt_child_filter_tracks_module() {
    let (_, module) = module_path!().split_once("::").expect("test module path");
    assert_eq!(
        vt_restore_test_name(),
        format!("{module}::vt_raw_terminal_restores_both_console_modes")
    );
}

#[test]
fn vt_raw_terminal_restores_both_console_modes() {
    if !super::windows_version::current_windows_version()
        .is_some_and(super::windows_version::supports_scoped_vt_input)
    {
        eprintln!("VT restoration acceptance requires a supported Windows build");
        return;
    }
    if std::env::var_os(CONSOLE_MODE_CHILD_ENV).is_none() {
        use std::thread;
        use std::time::{Duration, Instant};
        let receipt = child_selection::ChildReceipt::new();
        let executable = rmux_os::build_identity::resolve(
            std::env::current_exe()
                .expect("current test executable")
                .as_os_str(),
        )
        .expect("verify VT restoration child before spawn");
        let mut child = rmux_pty::ChildCommand::new(executable)
            .args([
                "--exact",
                &vt_restore_test_name(),
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CONSOLE_MODE_CHILD_ENV, "1")
            .env(child_selection::RECEIPT_ENV, receipt.path())
            .spawn()
            .expect("spawn VT restoration child in real ConPTY");
        let terminal_io = child.master().try_clone_io().expect("clone ConPTY I/O");
        // The parent is the ConPTY's hosting terminal: passthrough forwards
        // the child's DECRQM here, so the reply must come back on the input.
        let drain = thread::spawn(move || {
            let mut terminal = decrqm_terminal::ReportingModeTerminal::default();
            let mut bytes = Vec::new();
            let mut reply_error = None;
            let mut buffer = [0; 4096];
            while let Ok(count) = terminal_io.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
                let replies = terminal.feed(&buffer[..count]);
                if !replies.is_empty() && reply_error.is_none() {
                    reply_error = terminal_io.write_all(&replies).err();
                }
            }
            (bytes, reply_error, terminal.answered())
        });
        // Longer than the child's own 5 s DECRQM bound, so a missing reply
        // fails inside the child with its phase and bytes, not as a timeout.
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.child_mut().try_wait().expect("query VT child") {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            thread::yield_now();
        };
        let _ = child.child_mut().terminate_forcefully();
        let _ = child.child_mut().wait();
        child.child().close_pseudoconsole();
        drop(child);
        let (bytes, reply_error, answered) = drain.join().expect("drain VT restoration output");
        eprintln!(
            "native-vt-restoration-output={:?} reply-error={reply_error:?} decrqm-answered={answered}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(
            status.is_some_and(|status| status.success()),
            "VT restoration child failed"
        );
        receipt
            .verify(
                status.is_some_and(|status| status.success()),
                &vt_restore_test_name(),
            )
            .expect("ConPTY child must complete its exact test once");
        let pinned = format!(
            "native-reporting-oracle={:?}",
            ReportingOracle::HostPinnedSet
        );
        let pinned = pinned.as_bytes();
        assert!(
            answered > 0 || !bytes.windows(pinned.len()).any(|part| part == pinned),
            "only a hosting terminal that answered DECRQM can pin the reporting modes"
        );
        return;
    }
    let conin = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .expect("open isolated console input");
    let conout = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .expect("open isolated console output");
    let input = conin.as_raw_handle() as HANDLE;
    let output = conout.as_raw_handle() as HANDLE;
    unsafe {
        // SAFETY: these owned console handles outlive every guard below.
        assert_ne!(
            SetStdHandle(STD_INPUT_HANDLE, input),
            0,
            "set console stdin"
        );
        assert_ne!(
            SetStdHandle(STD_OUTPUT_HANDLE, output),
            0,
            "set console stdout"
        );
    }
    let original = (read_console_mode(input), read_console_mode(output));
    let initial = (
        original.0 & !ENABLE_VIRTUAL_TERMINAL_INPUT,
        original.1 & !ENABLE_VIRTUAL_TERMINAL_PROCESSING & !DISABLE_NEWLINE_AUTO_RETURN,
    );
    write_console_mode(input, initial.0);
    write_console_mode(output, initial.1);
    // Calibrate the query under non-VT output: enabling VT temporarily for a
    // read-only DECRQM must not reset either private reporting mode.
    set_reporting_for_query_calibration(output, true);
    assert_eq!(query_reporting_modes(input, output), (true, true));
    // Under passthrough the hosting terminal answers, and conhost keeps both
    // modes set there; only a console that answers itself shows the reset.
    set_reporting_for_query_calibration(output, false);
    let oracle = ReportingOracle::after_explicit_reset(query_reporting_modes(input, output))
        .expect("an explicit reset must answer both reporting modes alike");
    eprintln!("native-reporting-oracle={oracle:?}");
    let assert_modes =
        |expected, phase| assert_native_modes(input, output, oracle, expected, phase);
    assert_modes(initial, "before-enter");

    let mut expected_raw = None;
    for writer_first in [true, false] {
        let terminal = RawTerminal::enter().expect("enter VT raw terminal");
        let stdout = AttachStdout::for_managed_terminal(io::sink());
        let raw = *expected_raw
            .get_or_insert_with(|| (read_console_mode(input), read_console_mode(output)));
        assert_ne!(
            raw.0 & ENABLE_VIRTUAL_TERMINAL_INPUT,
            0,
            "exercise real VT input branch"
        );
        assert_ne!(
            raw.1 & ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            0,
            "enable VT output"
        );
        assert!(super::terminal::vt_input_context_active(input));
        assert_modes(raw, "after-enter");
        terminal.restore().expect("explicit VT restore");
        assert_modes(initial, "after-explicit-restore");
        assert!(!super::terminal::vt_input_context_active(input));
        terminal.restore().expect("idempotent explicit restore");
        assert_modes(initial, "after-second-restore");
        terminal
            .suspend_self()
            .expect("restore then reapply through suspend path");
        assert_modes(raw, "after-reapply");
        assert!(super::terminal::vt_input_context_active(input));
        if writer_first {
            drop(stdout);
            drop(terminal);
        } else {
            drop(terminal);
            assert_modes(initial, "after-terminal-drop-before-writer");
            drop(stdout);
        }
        assert_modes(initial, "after-drop");
        assert!(!super::terminal::vt_input_context_active(input));
    }
    let terminal = RawTerminal::enter().expect("re-enter VT raw terminal");
    assert_modes(expected_raw.expect("captured raw modes"), "after-reentry");
    drop(terminal);
    assert_modes(initial, "after-reentry-drop");
    assert!(!super::terminal::vt_input_context_active(input));
    write_console_mode(input, original.0);
    write_console_mode(output, original.1);
    child_selection::record_completion(&vt_restore_test_name());
}

fn assert_native_modes(
    input: HANDLE,
    output: HANDLE,
    oracle: ReportingOracle,
    expected: (u32, u32),
    phase: &str,
) {
    let actual = (read_console_mode(input), read_console_mode(output));
    assert_eq!(actual, expected, "exact input/output modes at {phase}");
    let reporting = query_reporting_modes(input, output);
    let enabled = expected.0 & ENABLE_VIRTUAL_TERMINAL_INPUT != 0;
    eprintln!(
        "native-console-modes phase={phase} input={} output={} win32={} focus={}",
        actual.0, actual.1, reporting.0, reporting.1
    );
    assert_eq!(
        reporting,
        oracle.expected(enabled),
        "{oracle:?} private reporting state at {phase}"
    );
    assert_eq!(
        (read_console_mode(input), read_console_mode(output)),
        expected,
        "read-only query must preserve exact input/output modes at {phase}"
    );
}

fn set_reporting_for_query_calibration(output: HANDLE, enabled: bool) {
    let original = read_console_mode(output);
    write_console_mode(output, original | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    write_console_sequence(
        output,
        if enabled {
            "\x1b[?1004h\x1b[?9001h"
        } else {
            "\x1b[?9001l\x1b[?1004l"
        },
    );
    write_console_mode(output, original);
}

fn write_console_sequence(output: HANDLE, sequence: &str) {
    let wide: Vec<u16> = sequence.encode_utf16().collect();
    let mut written = 0;
    let ok = unsafe {
        // SAFETY: output is live; wide and written remain valid for this call.
        WriteConsoleW(
            output,
            wide.as_ptr().cast(),
            wide.len() as u32,
            &mut written,
            std::ptr::null(),
        )
    };
    assert_ne!(
        ok,
        0,
        "WriteConsoleW failed: {}",
        io::Error::last_os_error()
    );
    assert_eq!(
        written as usize,
        wide.len(),
        "complete native control sequence"
    );
}

fn query_reporting_modes(input: HANDLE, output: HANDLE) -> (bool, bool) {
    use std::time::{Duration, Instant};
    let original = read_console_mode(output);
    write_console_mode(output, original | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    write_console_sequence(output, "\x1b[?9001$p\x1b[?1004$p");
    write_console_mode(output, original);
    let end = Instant::now() + Duration::from_secs(5);
    let mut bytes = Vec::new();
    loop {
        let value = |mode| {
            [true, false].into_iter().find(|enabled| {
                let expected = format!("\x1b[?{mode};{}$y", if *enabled { 1 } else { 2 });
                bytes
                    .windows(expected.len())
                    .any(|part| part == expected.as_bytes())
            })
        };
        if let (Some(win32), Some(focus)) = (value(9001), value(1004)) {
            eprintln!("native-decrpm-response={bytes:?}");
            return (win32, focus);
        }
        assert!(Instant::now() < end, "native DECRQM timed out: {bytes:?}");
        let mut available = 0;
        let ok = unsafe {
            // SAFETY: input is a live CONIN$ handle; available is writable.
            GetNumberOfConsoleInputEvents(input, &mut available)
        };
        assert_ne!(ok, 0, "query native input event count");
        if available == 0 {
            std::thread::yield_now();
            continue;
        }
        let mut records: [INPUT_RECORD; 128] = unsafe {
            // SAFETY: this writable Win32 input buffer is filled before use.
            std::mem::zeroed()
        };
        let mut count = 0;
        let ok = unsafe {
            // SAFETY: records holds 128 writable records; count is writable.
            ReadConsoleInputW(input, records.as_mut_ptr(), available.min(128), &mut count)
        };
        assert_ne!(ok, 0, "read native mode-query response");
        for record in &records[..count as usize] {
            if record.EventType == KEY_EVENT as u16 {
                let key = unsafe {
                    // SAFETY: KEY_EVENT selects this INPUT_RECORD union member.
                    record.Event.KeyEvent
                };
                if key.bKeyDown != 0 {
                    let ch = unsafe {
                        // SAFETY: ReadConsoleInputW returns the Unicode member.
                        key.uChar.UnicodeChar
                    };
                    if let Ok(byte) = u8::try_from(ch) {
                        bytes.push(byte);
                    }
                }
            }
        }
    }
}

fn exercise_redirected_stdout(output_handle: HANDLE, initial_mode: u32) {
    let mut pipe_read: HANDLE = std::ptr::null_mut();
    let mut pipe_write: HANDLE = std::ptr::null_mut();
    let pipe_created = unsafe {
        // SAFETY: both handle slots are writable and this isolated child uses
        // the default local pipe security descriptor.
        CreatePipe(&mut pipe_read, &mut pipe_write, std::ptr::null_mut(), 0)
    };
    assert_ne!(
        pipe_created,
        0,
        "CreatePipe failed: {}",
        io::Error::last_os_error()
    );
    let _pipe_read = unsafe {
        // SAFETY: pipe_read is uniquely owned after CreatePipe succeeds.
        OwnedHandle::from_raw_handle(pipe_read)
    };
    let pipe_write = unsafe {
        // SAFETY: pipe_write is uniquely owned after CreatePipe succeeds.
        OwnedHandle::from_raw_handle(pipe_write)
    };
    let redirect_stdout = unsafe {
        // SAFETY: pipe_write remains live through the redirected-output
        // lifecycle exercised below.
        SetStdHandle(STD_OUTPUT_HANDLE, pipe_write.as_raw_handle() as HANDLE)
    };
    assert_ne!(
        redirect_stdout,
        0,
        "redirect SetStdHandle failed: {}",
        io::Error::last_os_error()
    );
    write_console_mode(output_handle, initial_mode);

    let terminal = RawTerminal::enter().expect("enter with redirected stdout");
    let fallback = RecordingWriter::default();
    let observed = fallback.clone();
    let mut stdout = AttachStdout::for_managed_terminal(fallback);
    let payload = b"redirected-attach-output";
    stdout
        .write_all(payload)
        .expect("write redirected attach output");
    stdout.flush().expect("flush redirected attach output");
    assert_eq!(
        observed.snapshot(),
        payload,
        "a valid non-console stdout must receive attach output through the caller's writer"
    );
    drop(stdout);
    drop(terminal);
    assert_eq!(
        read_console_mode(output_handle),
        initial_mode,
        "redirected stdout handling must not leave the outer console mode changed"
    );
}

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl RecordingWriter {
    fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().expect("recording writer mutex").clone()
    }
}

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("recording writer mutex poisoned"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_console_mode(handle: HANDLE) -> u32 {
    let mut mode = 0_u32;
    let ok = unsafe {
        // SAFETY: handle is a live CONOUT$ handle and mode is writable.
        GetConsoleMode(handle, &mut mode)
    };
    assert_ne!(
        ok,
        0,
        "GetConsoleMode failed: {}",
        io::Error::last_os_error()
    );
    mode
}

fn write_console_mode(handle: HANDLE, mode: u32) {
    let ok = unsafe {
        // SAFETY: handle is a live CONOUT$ handle and mode preserves the
        // console's existing flags except for explicit VT/raw toggles.
        SetConsoleMode(handle, mode)
    };
    assert_ne!(
        ok,
        0,
        "SetConsoleMode failed: {}",
        io::Error::last_os_error()
    );
}
