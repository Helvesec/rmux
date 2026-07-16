use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Command;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    AllocConsole, FreeConsole, GetConsoleMode, SetConsoleMode, SetStdHandle,
    DISABLE_NEWLINE_AUTO_RETURN, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;

use super::{output::AttachStdout, RawTerminal};

const CONSOLE_MODE_CHILD_ENV: &str = "RMUX_INTERNAL_CONSOLE_MODE_OWNER_TEST_CHILD";
const CONSOLE_MODE_TEST_NAME: &str =
    "attach::console_mode_ownership_tests::managed_attach_stdout_has_single_console_mode_owner";

#[test]
fn managed_attach_stdout_has_single_console_mode_owner() {
    if std::env::var_os(CONSOLE_MODE_CHILD_ENV).is_none() {
        let status = Command::new(std::env::current_exe().expect("current test executable"))
            .args(["--exact", CONSOLE_MODE_TEST_NAME, "--nocapture"])
            .env(CONSOLE_MODE_CHILD_ENV, "1")
            .status()
            .expect("spawn isolated console-mode test child");
        assert!(status.success(), "console-mode test child failed: {status}");
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
