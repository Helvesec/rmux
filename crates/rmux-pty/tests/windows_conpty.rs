#![cfg(windows)]

use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, PtyMaster, PtyPair, TerminalSize};

#[test]
fn conpty_pair_opens_resizes_and_clones_master() -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(TerminalSize::new(100, 30))?;
    assert_eq!(pair.master().size()?, TerminalSize::new(100, 30));

    pair.master().resize(TerminalSize::new(120, 40))?;
    assert_eq!(pair.master().size()?, TerminalSize::new(120, 40));

    let clone = pair.master().try_clone()?;
    assert_eq!(clone.size()?, TerminalSize::new(120, 40));
    Ok(())
}

#[test]
fn conpty_spawn_reads_child_output_and_waits() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/C", "echo RMUX_SPAWN_OK"])
        .size(TerminalSize::new(100, 30))
        .spawn()?;

    let output = read_until(spawned.master(), b"RMUX_SPAWN_OK", Duration::from_secs(2))?;
    let status = spawned.child_mut().wait()?;

    assert!(status.success());
    assert!(
        String::from_utf8_lossy(&output).contains("RMUX_SPAWN_OK"),
        "expected marker in ConPTY output, got {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(spawned.child_mut().try_wait()?.is_some());
    Ok(())
}

#[test]
fn conpty_force_kill_reaps_child() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    spawned.child().terminate_forcefully()?;
    let status = spawned.child_mut().wait()?;
    assert!(!status.success());
    assert!(spawned.child_mut().try_wait()?.is_some());
    Ok(())
}

#[test]
fn conpty_resize_after_child_exit_is_not_fatal() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/C", "exit 0"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    assert!(spawned.child_mut().wait()?.success());
    spawned.master().resize(TerminalSize::new(90, 25))?;
    Ok(())
}

fn read_until(
    master: &PtyMaster,
    needle: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let io = master.try_clone_io()?;
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];

    while Instant::now() < deadline {
        let bytes_read = io.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..bytes_read]);
        if output.windows(needle.len()).any(|window| window == needle) {
            return Ok(output);
        }
    }

    Ok(output)
}
