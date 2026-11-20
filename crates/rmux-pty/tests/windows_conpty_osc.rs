#![cfg(windows)]

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, TerminalSize};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

const HELPER: &str = "RMUX_TEST_OSC_BOUNDARY_HELPER";
const OUTPUT: &[u8] = b"OSC_BEFORE\x1b[c\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07OSC_AFTER";
const INPUT: &[u8] = b"keyboard\x1b[?62;22c\x1b]10;rgb:1212/3434/5656\x07\x1b]11;rgb:7878/9a9a/bcbc\x1b\\\x1b]12;rgb:dede/f0f0/1212\x07\x1b[200~paste\x1b[201~";
const DONE: &[u8] = b"OSC_ROUNDTRIP_OK";

#[test]
fn osc_boundary_helper() -> io::Result<()> {
    if std::env::var_os(HELPER).is_none() {
        return Ok(());
    }
    for (kind, add, remove) in [
        (
            STD_INPUT_HANDLE,
            ENABLE_VIRTUAL_TERMINAL_INPUT,
            ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT,
        ),
        (
            STD_OUTPUT_HANDLE,
            ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            0,
        ),
    ] {
        // SAFETY: standard handles belong to this helper's console, and mode
        // points to writable storage. No handle ownership is transferred.
        unsafe {
            let handle = GetStdHandle(kind);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0
                || SetConsoleMode(handle, (mode & !remove) | add) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
    }
    io::stdout().write_all(OUTPUT)?;
    io::stdout().flush()?;
    let mut received = vec![0; INPUT.len()];
    io::stdin().read_exact(&mut received)?;
    assert_eq!(
        received, INPUT,
        "terminal replies, keyboard and paste must reach application"
    );
    io::stdout().write_all(DONE)?;
    io::stdout().flush()
}

#[test]
#[ignore = "requires Microsoft ConPTY >= 1.24.2607.10001 paired next to the test executable"]
fn conpty_preserves_colour_queries_replies_and_eof() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = ChildCommand::new(std::env::current_exe()?)
        .args(["--exact", "osc_boundary_helper", "--nocapture"])
        .env(HELPER, "1")
        .size(TerminalSize::new(100, 30))
        .spawn()?;
    let reader = child.master().try_clone_io()?;
    let (tx, rx) = mpsc::channel();
    let read_thread = thread::spawn(move || {
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = tx.send(Ok(Vec::new()));
                    break;
                }
                Ok(n) => {
                    if tx.send(Ok(buffer[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        while !contains(&output, OUTPUT) {
            let bytes = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
            if bytes.is_empty() {
                return Err("EOF before OSC queries".into());
            }
            output.extend(bytes);
        }
        // Query visibility is checked separately from the input capability.
        assert!(child.master().preserves_verbatim_input());
        child.master().resize(TerminalSize::new(110, 35))?;
        child
            .master()
            .write_all_with_timeout(INPUT, Duration::from_secs(2))?;
        while !contains(&output, DONE) {
            let bytes = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
            if bytes.is_empty() {
                return Err("EOF before application round trip".into());
            }
            output.extend(bytes);
        }
        loop {
            if let Some(status) = child.child_mut().try_wait()? {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                return Err("helper exit deadline".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        child.child().close_pseudoconsole();
        loop {
            if rx.recv_timeout(Duration::from_secs(3))??.is_empty() {
                break;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.child().terminate_forcefully();
        let _ = child.child_mut().wait();
        child.child().close_pseudoconsole();
    }
    // The production close releases the output pipe; do not detach a reader.
    read_thread.join().map_err(|_| "output reader panicked")?;
    result
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}
