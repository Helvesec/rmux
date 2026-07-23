#![cfg(unix)]

#[cfg(target_os = "linux")]
use std::fs;
use std::fs::File;
use std::io::{self, Write};
#[cfg(target_os = "linux")]
use std::os::unix::fs::FileTypeExt;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, ProcessId, PtyPair, Signal, TerminalSize};

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
struct ProcessStat {
    pgrp: i32,
    session: i32,
    tty_nr: i32,
    tpgid: i32,
}

fn read_exact_from_master(
    master: &rmux_pty::PtyMaster,
    len: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let io = master.try_clone_io()?;
    let mut buffer = vec![0_u8; len];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut offset = 0;
    while offset < buffer.len() {
        offset += read_master_with_deadline(&io, &mut buffer[offset..], deadline)?;
    }
    Ok(buffer)
}

fn read_line_from_master(
    master: &rmux_pty::PtyMaster,
) -> Result<String, Box<dyn std::error::Error>> {
    let io = master.try_clone_io()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        read_master_with_deadline(&io, &mut byte, deadline)?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(String::from_utf8(line)?);
        }
    }
}

fn read_master_with_deadline(
    io: &rmux_pty::PtyIo,
    buffer: &mut [u8],
    deadline: Instant,
) -> io::Result<usize> {
    loop {
        match io.try_read(buffer) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(bytes_read) => return Ok(bytes_read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for PTY output",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pid_raw(pid: ProcessId) -> i32 {
    i32::try_from(pid.as_u32()).expect("test pid must fit in i32")
}

#[cfg(target_os = "linux")]
fn read_process_stat(pid: ProcessId) -> Result<ProcessStat, Box<dyn std::error::Error>> {
    let stat_path = format!("/proc/{}/stat", pid_raw(pid));
    let stat = fs::read_to_string(stat_path)?;
    let command_end = stat
        .rfind(')')
        .ok_or_else(|| "missing command terminator in /proc stat".to_string())?;
    let fields: Vec<&str> = stat[command_end + 2..].split_whitespace().collect();

    Ok(ProcessStat {
        pgrp: fields
            .get(2)
            .ok_or_else(|| "missing pgrp field in /proc stat".to_string())?
            .parse()?,
        session: fields
            .get(3)
            .ok_or_else(|| "missing session field in /proc stat".to_string())?
            .parse()?,
        tty_nr: fields
            .get(4)
            .ok_or_else(|| "missing tty_nr field in /proc stat".to_string())?
            .parse()?,
        tpgid: fields
            .get(5)
            .ok_or_else(|| "missing tpgid field in /proc stat".to_string())?
            .parse()?,
    })
}

#[cfg(target_os = "linux")]
fn child_fd_path(pid: ProcessId, fd: u8) -> PathBuf {
    format!("/proc/{}/fd/{fd}", pid_raw(pid)).into()
}

#[cfg(target_os = "linux")]
fn process_exists(pid: ProcessId) -> bool {
    fs::metadata(format!("/proc/{}", pid_raw(pid))).is_ok()
}

#[cfg(target_os = "macos")]
fn process_exists(pid: ProcessId) -> bool {
    // SAFETY: `kill(pid, 0)` does not deliver a signal; it only checks whether
    // the process exists and is visible to the current user.
    let result = unsafe { libc::kill(pid_raw(pid), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(target_os = "linux")]
fn is_wsl_kernel() -> bool {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_for_exit(
    child: &mut rmux_pty::PtyChild,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }

        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn allocated_pair_round_trips_resized_terminal_size() -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(TerminalSize::new(120, 40))?;
    assert_eq!(pair.master().size()?, TerminalSize::new(120, 40));

    pair.master().resize(TerminalSize::new(200, 50))?;
    assert_eq!(pair.master().size()?, TerminalSize::new(200, 50));
    Ok(())
}

#[test]
fn allocated_master_is_nonblocking_before_exposure() -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open()?;
    let mut byte = [0_u8; 1];

    let error = pair
        .master()
        .io()
        .try_read(&mut byte)
        .expect_err("idle master read must not block");

    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn spawned_child_is_session_and_foreground_group_leader() -> Result<(), Box<dyn std::error::Error>>
{
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .size(TerminalSize::new(88, 28))
        .spawn()?;

    let pid = spawned.child().pid();
    let stat = read_process_stat(pid)?;
    let fd0 = fs::read_link(child_fd_path(pid, 0))?;
    let fd0_metadata = fs::metadata(child_fd_path(pid, 0))?;

    assert_eq!(stat.session, pid_raw(pid));
    assert_eq!(stat.pgrp, pid_raw(pid));
    if !is_wsl_kernel() {
        assert_eq!(stat.tpgid, pid_raw(pid));
    }
    assert_ne!(stat.tty_nr, 0);
    assert!(fd0.starts_with("/dev/pts/"));
    assert!(fd0_metadata.file_type().is_char_device());

    spawned.child().kill(Signal::KILL)?;
    let status = spawned.child_mut().wait()?;
    assert!(!status.success());

    Ok(())
}

#[test]
fn spawned_child_gets_a_controlling_tty() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "tty"])
        .size(TerminalSize::new(88, 28))
        .spawn()?;

    let tty = read_line_from_master(spawned.master())?;
    let status = spawned.child_mut().wait()?;

    assert!(status.success());
    assert!(
        tty.trim().starts_with("/dev/"),
        "expected child to report a real tty, got {tty:?}"
    );

    Ok(())
}

#[test]
fn spawned_child_reads_and_writes_through_master() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "stty raw -echo; printf READY; exec cat"])
        .size(TerminalSize::new(90, 30))
        .spawn()?;

    assert_eq!(read_exact_from_master(spawned.master(), 5)?, b"READY");

    let payload = b"hello over pty";
    let writer_io = spawned.master().try_clone_io()?;
    let mut writer = File::from(writer_io.as_fd().try_clone_to_owned()?);
    writer.write_all(payload)?;
    writer.flush()?;

    assert_eq!(
        read_exact_from_master(spawned.master(), payload.len())?,
        payload
    );
    spawned.master().resize(TerminalSize::new(101, 41))?;
    assert_eq!(spawned.master().size()?, TerminalSize::new(101, 41));

    spawned.child().kill(Signal::TERM)?;
    let status = spawned.child_mut().wait()?;
    assert!(!status.success());

    Ok(())
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_all_times_out_when_child_stops_draining_stdin() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "stty raw -echo; printf READY; sleep 30"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    assert_eq!(read_exact_from_master(spawned.master(), 5)?, b"READY");

    let payload = vec![b'x'; 16 * 1024 * 1024];
    let started = std::time::Instant::now();

    let error = spawned
        .master()
        .write_all(&payload)
        .expect_err("non-draining child should apply PTY write backpressure");

    let _ = spawned.child().kill(Signal::KILL);
    let _ = spawned.child_mut().wait();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "write timeout should be bounded, elapsed={:?}",
        started.elapsed()
    );

    Ok(())
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_timeout_resets_while_child_drains_slowly() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args([
            "-c",
            "stty raw -echo; printf READY; while :; do dd bs=1024 count=1 of=/dev/null 2>/dev/null; sleep 0.05; done",
        ])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    assert_eq!(read_exact_from_master(spawned.master(), 5)?, b"READY");

    let payload = vec![b'x'; 64 * 1024];
    let timeout = Duration::from_secs(1);
    let started = std::time::Instant::now();
    spawned
        .master()
        .write_all_with_timeout(&payload, timeout)
        .expect("continuous progress should keep the write alive");

    let _ = spawned.child().kill(Signal::KILL);
    let _ = spawned.child_mut().wait();

    assert!(
        started.elapsed() > timeout,
        "test must outlive one inactivity timeout, elapsed={:?}",
        started.elapsed()
    );

    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn kill_terminates_the_pty_process_group() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args([
            "-c",
            "stty raw -echo; sleep 30 & printf '%s\\n' \"$!\"; wait",
        ])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    let descendant_pid = read_line_from_master(spawned.master())?
        .trim()
        .parse::<i32>()?;
    let descendant_pid = ProcessId::new(u32::try_from(descendant_pid)?)?;
    let descendant_stat = read_process_stat(descendant_pid)?;

    assert_eq!(descendant_stat.pgrp, pid_raw(spawned.child().pid()));

    spawned.child().kill(Signal::TERM)?;
    let deadline = std::time::Instant::now() + Duration::from_millis(250);
    while process_exists(descendant_pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    if process_exists(descendant_pid) {
        let _ = spawned.child().kill(Signal::KILL);
        let _ = spawned.child_mut().wait();
    }

    assert!(!process_exists(descendant_pid), "descendant survived TERM");

    let status = wait_for_exit(spawned.child_mut(), Duration::from_millis(250))?;
    if status.is_none() {
        let _ = spawned.child().kill(Signal::KILL);
        let _ = spawned.child_mut().wait();
    }
    assert!(status.is_some(), "PTY leader did not exit after TERM");

    Ok(())
}

#[test]
#[cfg(target_os = "macos")]
fn kill_terminates_the_macos_pty_process_group() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args([
            "-c",
            "stty raw -echo; sleep 30 & printf '%s\\n' \"$!\"; wait",
        ])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    let descendant_pid = read_line_from_master(spawned.master())?
        .trim()
        .parse::<i32>()?;
    let descendant_pid = ProcessId::new(u32::try_from(descendant_pid)?)?;

    spawned.child().kill(Signal::TERM)?;
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while process_exists(descendant_pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    if process_exists(descendant_pid) {
        let _ = spawned.child().kill(Signal::KILL);
        let _ = spawned.child_mut().wait();
    }

    assert!(
        !process_exists(descendant_pid),
        "macOS PTY descendant survived TERM"
    );

    let status = wait_for_exit(spawned.child_mut(), Duration::from_millis(500))?;
    if status.is_none() {
        let _ = spawned.child().kill(Signal::KILL);
        let _ = spawned.child_mut().wait();
    }
    assert!(status.is_some(), "macOS PTY leader did not exit after TERM");

    Ok(())
}

#[test]
fn spawned_child_receives_environment_and_reaps_cleanly() -> Result<(), Box<dyn std::error::Error>>
{
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "printf %s \"$RMUX_TEST_VALUE\""])
        .env("RMUX_TEST_VALUE", "visible")
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    assert_eq!(read_exact_from_master(spawned.master(), 7)?, b"visible");
    let status = spawned.child_mut().wait()?;
    assert!(status.success());
    assert!(spawned.child_mut().try_wait()?.is_some());

    Ok(())
}

#[test]
fn wait_reports_child_exit_status() -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "exit 7"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    let status = spawned.child_mut().wait()?;
    assert_eq!(status.code(), Some(7));

    std::thread::sleep(Duration::from_millis(10));
    assert!(spawned.child_mut().try_wait()?.is_some());

    Ok(())
}
