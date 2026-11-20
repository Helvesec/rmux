#![cfg(unix)]

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
    // The child ends the line itself, even when `tty` fails: uutils `tty` 0.10
    // (Ubuntu 26.04) prints the name without the newline GNU `tty` writes.
    let mut spawned = ChildCommand::new("/bin/sh")
        .args([
            "-c",
            r#"name=$(tty); status=$?; printf '%s\n' "$name"; exit "$status""#,
        ])
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

// The pane reader asks the master whether a silent pane's process is gone. A
// status query (`#{pane_dead}`, send-keys, pipe-pane...) may reap that process
// through the child handle first; the master must keep answering that it exited
// instead of falling back to the startup retry budget.
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn session_leader_exit_stays_observable_after_its_child_handle_reaps_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "exit 7"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;
    let reader_io = spawned.master().try_clone_io()?;

    let status = wait_for_exit(spawned.child_mut(), Duration::from_secs(30))?
        .expect("`exit 7` ends its shell");
    assert_eq!(status.code(), Some(7));
    assert!(
        reader_io.session_leader_has_exited(),
        "a session leader reaped by try_wait must still read as exited"
    );
    let late_io = spawned.master().try_clone_io()?;
    assert!(late_io.session_leader_has_exited());

    Ok(())
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn session_leader_reads_alive_until_it_exits_then_stays_exited_after_wait(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .size(TerminalSize::new(80, 24))
        .spawn()?;
    let reader_io = spawned.master().try_clone_io()?;

    assert!(spawned.child_mut().try_wait()?.is_none());
    assert!(!reader_io.session_leader_has_exited());

    spawned.child().kill(Signal::KILL)?;
    let status = spawned.child_mut().wait()?;
    assert!(!status.success());
    assert!(
        reader_io.session_leader_has_exited(),
        "a session leader reaped by wait must still read as exited"
    );

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
fn queued_input_survives_a_child_busy_past_the_write_timeout_in_order(
) -> Result<(), Box<dyn std::error::Error>> {
    // R3512 H1: the child does not read for longer than the 2 s no-progress
    // deadline of `write_all`. Queued input must neither wait for it nor lose
    // the bytes the slave buffer could not take, and pushes from different
    // clones of the endpoint keep their order.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let output =
        std::env::temp_dir().join(format!("rmux-pty-queue-{}-{unique}", std::process::id()));
    let first = vec![b'a'; 192 * 1024];
    let second = vec![b'b'; 64 * 1024];
    let third = b"end".to_vec();
    let total = first.len() + second.len() + third.len();
    let mut spawned = ChildCommand::new("/bin/sh")
        .args([
            "-c",
            &format!(
                "stty raw -echo; printf READY; sleep 3; head -c {total} > \"$RMUX_TEST_OUT\"; printf DONE; sleep 30"
            ),
        ])
        .env("RMUX_TEST_OUT", &output)
        .size(TerminalSize::new(80, 24))
        .spawn()?;
    assert_eq!(read_exact_from_master(spawned.master(), 5)?, b"READY");

    let other_clone = spawned.master().try_clone_io()?;
    let started = Instant::now();
    spawned.master().queue_input(&first)?;
    other_clone.queue_input(&second)?;
    drop(other_clone);
    spawned.master().queue_input(&third)?;
    let queued_after = started.elapsed();

    let io = spawned.master().try_clone_io()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut done = [0_u8; 4];
    let mut offset = 0;
    let delivered = loop {
        if offset == done.len() {
            break Ok(());
        }
        match read_master_with_deadline(&io, &mut done[offset..], deadline) {
            Ok(read) => offset += read,
            Err(error) => break Err(error),
        }
    };
    let received = fs::read(&output).unwrap_or_default();
    let _ = spawned.child().kill(Signal::KILL);
    let _ = spawned.child_mut().wait();
    let _ = fs::remove_file(&output);

    assert!(
        queued_after < Duration::from_millis(500),
        "queueing input waited for the busy child: {queued_after:?}"
    );
    delivered?;
    assert_eq!(&done, b"DONE");
    assert_eq!(
        received.len(),
        total,
        "child received {} of {total} bytes",
        received.len()
    );
    let expected: Vec<u8> = [first, second, third].concat();
    assert!(
        received == expected,
        "queued input reached the child out of order"
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
fn blocked_write_resumes_when_child_frees_room_inside_its_backlog(
) -> Result<(), Box<dyn std::error::Error>> {
    // Linux wakes a PTY master blocked in poll(POLLOUT) only once the slave
    // line-discipline buffer (4095 bytes) drains below its unthrottle threshold.
    // The child reads 3 KiB, so room appears without that wakeup.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let trigger =
        std::env::temp_dir().join(format!("rmux-pty-drain-{}-{unique}", std::process::id()));
    let mut spawned = ChildCommand::new("/bin/sh")
        .args([
            "-c",
            "stty raw -echo; printf READY; while [ ! -e \"$RMUX_TEST_DRAIN\" ]; do sleep 0.01; done; dd bs=1024 count=3 of=/dev/null 2>/dev/null; sleep 30",
        ])
        .env("RMUX_TEST_DRAIN", &trigger)
        .size(TerminalSize::new(80, 24))
        .spawn()?;

    assert_eq!(read_exact_from_master(spawned.master(), 5)?, b"READY");

    let fill = spawned
        .master()
        .write_all_with_timeout(&vec![b'x'; 1024 * 1024], Duration::from_millis(200))
        .expect_err("a child that does not read must fill the PTY");
    assert_eq!(fill.kind(), io::ErrorKind::TimedOut);

    let release = {
        let trigger = trigger.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            File::create(trigger).map(drop)
        })
    };
    let started = Instant::now();
    let resumed = spawned
        .master()
        .write_all_with_timeout(&[b'y'; 256], Duration::from_secs(3));
    let waited = started.elapsed();
    let released = release.join().expect("trigger thread must not panic");

    let _ = spawned.child().kill(Signal::KILL);
    let _ = spawned.child_mut().wait();
    let _ = fs::remove_file(&trigger);

    released?;
    resumed.expect("room freed by the child must resume the blocked write");
    // poll re-reads readiness when its timeout expires, so only an early
    // resume proves the freed room, not the deadline, completed the write.
    assert!(
        waited < Duration::from_secs(2),
        "the write resumed only at its deadline, waited={waited:?}"
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
