#![cfg(windows)]

use std::error::Error;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

#[test]
fn console_mutex_serializes_without_a_filesystem_lock() -> Result<(), Box<dyn Error>> {
    const CHILD: &str = "RMUX_SERIAL_LOCK_REGRESSION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Change the temporary directory only in a child, never in parallel tests.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temporary = std::env::temp_dir().join(format!(
            "rmux-serial-regression-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&temporary)?;
        let output = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "console_mutex_serializes_without_a_filesystem_lock",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("TEMP", &temporary)
            .env("TMP", &temporary)
            .output()?;
        std::fs::remove_dir_all(&temporary)?;
        assert!(
            output.status.success(),
            "native lock regression failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }

    // Reproduce an uncreatable legacy lock path without touching other tests.
    // The old create_new lock returns ACCESS_DENIED for this directory.
    let legacy = std::env::temp_dir().join("rmux-windows-cli-integration.lock");
    std::fs::create_dir(&legacy)?;
    let legacy_error = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&legacy)
        .expect_err("the legacy file-lock acquisition must fail");
    assert_eq!(legacy_error.raw_os_error(), Some(5));
    let held = windows_cli_serial::acquire("serial-regression-owner")?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let contender = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let _guard = windows_cli_serial::acquire("serial-regression-contender").unwrap();
        acquired_tx.send(()).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(5))?;
    assert!(matches!(
        acquired_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(held);
    acquired_rx.recv_timeout(Duration::from_secs(310))?;
    contender.join().expect("contender panicked");
    std::fs::remove_dir(legacy)?;
    Ok(())
}
