#![cfg(windows)]

use std::env;
use std::io::BufRead;
use std::thread;
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, TerminalSize};

const INPUT_SMOKE_HELPER_ENV: &str = "RMUX_TEST_CONPTY_INPUT_SMOKE";
const INPUT_SMOKE: &str = "rmux-conpty-input-smoke";

#[test]
fn conpty_input_smoke_helper() {
    if env::var_os(INPUT_SMOKE_HELPER_ENV).is_none() {
        return;
    }

    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .expect("read ConPTY input");
    assert_eq!(line.trim_end_matches(['\r', '\n']), INPUT_SMOKE);
}

#[test]
fn conpty_delivers_normal_input_to_the_child() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new(env::current_exe()?)
        .args(["--exact", "conpty_input_smoke_helper", "--nocapture"])
        .env(INPUT_SMOKE_HELPER_ENV, "1")
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    spawned.master().write_all_with_timeout(
        format!("{INPUT_SMOKE}\r").as_bytes(),
        Duration::from_secs(2),
    )?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = spawned.child_mut().try_wait()? {
            assert!(status.success(), "ConPTY input helper failed: {status}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            spawned.child().terminate_forcefully()?;
            let _ = spawned.child_mut().wait()?;
            return Err("ConPTY input helper did not receive the smoke payload".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
