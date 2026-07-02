#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use common::{assert_success, CliHarness};

#[test]
fn new_session_sets_pty_winsize_before_shell_observes_terminal_size() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("pty-winsize-before-shell")?;
    let output_path = harness.tmpdir().join("stty-size.txt");
    let command = format!("stty size > {}; sleep 1", shell_quote(&output_path));

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "winsize",
        "-x",
        "101",
        "-y",
        "33",
        "--",
        "/bin/sh",
        "-c",
        &command,
    ])?);

    let rendered = wait_for_file(&output_path, Duration::from_secs(5))?;
    assert_eq!(
        rendered.trim(),
        "33 101",
        "shell observed wrong initial PTY size"
    );

    Ok(())
}

fn wait_for_file(path: &std::path::Path, timeout: Duration) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        match fs::read_to_string(path) {
            Ok(value) if !value.trim().is_empty() => return Ok(value),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(value) => return Ok(value),
            Err(error) => {
                return Err(format!("timed out waiting for {}: {error}", path.display()).into())
            }
        }
    }
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
