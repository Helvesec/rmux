use super::*;

#[test]
fn fd_path_rejects_negative_descriptors() {
    assert_eq!(fd_path(std::process::id(), -1), None);
}

#[test]
fn current_process_is_live() {
    assert_eq!(
        ProcessInspector
            .is_live(std::process::id())
            .expect("liveness query"),
        Some(true)
    );
    assert!(is_live(std::process::id()));
}

#[test]
fn current_process_path_is_available() {
    let path = current_path(std::process::id()).expect("current process cwd should be visible");
    assert!(!path.is_empty());
}

#[test]
fn current_process_command_name_is_available() {
    let name = command_name(std::process::id()).expect("current process command should be visible");
    assert!(!name.is_empty());
}

#[test]
fn current_process_environment_is_available() {
    let environment =
        environment(std::process::id()).expect("current process environment should be visible");
    assert!(!environment.is_empty());
}

#[cfg(windows)]
#[test]
fn windows_reports_exited_process_as_dead_even_with_exit_code_259() {
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "exit", "259"])
        .spawn()
        .expect("spawn exit-code helper");
    let pid = child.id();

    loop {
        if child.try_wait().expect("poll helper").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert_eq!(
        ProcessInspector.is_live(pid).expect("liveness query"),
        Some(false)
    );
}

#[cfg(windows)]
#[test]
fn windows_reports_unavailable_fd_path_as_ok_none() {
    assert_eq!(
        ProcessInspector
            .fd_path(std::process::id(), 0)
            .expect("fd path query should not fail"),
        None
    );
}

#[cfg(windows)]
#[test]
fn windows_child_environment_is_visible() {
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "ping -n 6 127.0.0.1 >NUL"])
        .env("RMUX_OS_ENV_SMOKE", "visible")
        .spawn()
        .expect("spawn environment helper");
    let pid = child.id();

    for _ in 0..40 {
        let environment = ProcessInspector
            .environment(pid)
            .expect("environment query should not fail");
        if environment
            .as_ref()
            .and_then(|values| values.get("RMUX_OS_ENV_SMOKE"))
            .is_some_and(|value| value == "visible")
        {
            child.kill().ok();
            child.wait().ok();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    child.kill().ok();
    child.wait().ok();
    panic!("child environment did not become visible");
}

#[cfg(windows)]
#[test]
fn windows_child_current_path_is_visible() {
    let expected = std::env::current_dir()
        .expect("current dir")
        .to_string_lossy()
        .into_owned();
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "ping -n 6 127.0.0.1 >NUL"])
        .spawn()
        .expect("spawn cwd helper");
    let pid = child.id();

    for _ in 0..40 {
        let path = ProcessInspector
            .current_path(pid)
            .expect("current path query should not fail");
        if path
            .as_deref()
            .is_some_and(|path| windows_paths_match(path, &expected))
        {
            child.kill().ok();
            child.wait().ok();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    child.kill().ok();
    child.wait().ok();
    panic!("child cwd did not become visible");
}

#[cfg(windows)]
fn windows_paths_match(actual: &str, expected: &str) -> bool {
    fn normalize(path: &str) -> String {
        path.replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    }
    normalize(actual) == normalize(expected)
}

#[test]
fn parses_nul_separated_environment() {
    let environment = environment_from_nul_entries(b"A=1\0B=two\0\0").expect("environment");

    assert_eq!(environment.get("A").map(String::as_str), Some("1"));
    assert_eq!(environment.get("B").map(String::as_str), Some("two"));
}

#[cfg(unix)]
#[test]
fn winch_foreground_pgrp_returns_false_for_non_tty_fd() {
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};
    // A pipe has no controlling pgrp, so `tcgetpgrp` returns -1 and
    // the helper should bail without attempting `killpg`.
    let mut fds = [0; 2];
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(result, 0, "create pipe");
    let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let _write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    assert!(!unix::winch_foreground_pgrp(read_end.as_fd()));
}

#[cfg(unix)]
#[test]
fn winch_foreground_pgrp_delivers_signal_to_pty_session_leader() {
    use std::io::Read;
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // Allocate a PTY pair. We hold the master; the spawned shell will
    // adopt the slave as its controlling terminal so the kernel knows
    // who to deliver SIGWINCH to.
    let mut master_raw: libc::c_int = -1;
    let mut slave_raw: libc::c_int = -1;
    let opened = unsafe {
        libc::openpty(
            &mut master_raw,
            &mut slave_raw,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(opened, 0, "openpty");
    let master = unsafe { OwnedFd::from_raw_fd(master_raw) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_raw) };
    let slave_stdin = slave.try_clone().expect("clone slave for stdin");
    let slave_stdout = slave.try_clone().expect("clone slave for stdout");
    let slave_stderr = slave;

    // The shell installs a SIGWINCH trap that writes a sentinel to
    // stdout (= the slave PTY), then loops. `sleep 0.05` keeps the
    // wait short so the trap fires promptly; the outer loop bounds
    // child lifetime in case the test asserts before reaching kill.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("trap 'printf WINCHED' WINCH; i=0; while [ \"$i\" -lt 100 ]; do sleep 0.05; i=$((i+1)); done");
    cmd.stdin(Stdio::from(slave_stdin))
        .stdout(Stdio::from(slave_stdout))
        .stderr(Stdio::from(slave_stderr));
    unsafe {
        // setsid + TIOCSCTTY in the child so the slave becomes its
        // controlling terminal and the kernel knows who to deliver
        // SIGWINCH to when we killpg the slave's fg pgrp.
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn shell under pty");

    // Wait until the shell has the slave as its foreground pgrp.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut pgrp = None;
    while pgrp.is_none() {
        pgrp = unix::foreground_pid(master.as_fd());
        if pgrp.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let exited = child.try_wait().expect("try_wait").is_some();
            child.kill().ok();
            child.wait().ok();
            panic!(
                "child shell did not claim foreground pgrp within 2s (child exited prematurely: {exited})"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let fg_pgrp = pgrp.expect("fg pgrp set above");
    // Give the shell a moment to install its WINCH trap after the
    // pgrp is set — the foreground pgrp is set by setsid+TIOCSCTTY,
    // which runs before the shell parses `trap '...' WINCH`.
    std::thread::sleep(Duration::from_millis(100));

    assert!(
        unix::winch_foreground_pgrp(master.as_fd()),
        "killpg should succeed against a live fg pgrp (pgrp={fg_pgrp}, child={})",
        child.id()
    );

    // Read until we see the sentinel. The master is blocking, so use a
    // background-thread + recv pattern with a deadline.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let master_for_reader = master.try_clone().expect("clone master");
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(master_for_reader);
        let mut buffer = [0_u8; 256];
        loop {
            match file.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut collected = Vec::new();
    let read_deadline = Instant::now() + Duration::from_secs(3);
    while !collected.windows(7).any(|window| window == b"WINCHED") {
        let remaining = read_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    let saw_sentinel = collected.windows(7).any(|window| window == b"WINCHED");
    child.kill().ok();
    child.wait().ok();
    assert!(
        saw_sentinel,
        "expected SIGWINCH sentinel from shell; got: {:?}",
        String::from_utf8_lossy(&collected),
    );
}

#[cfg(target_os = "macos")]
#[test]
fn parses_macos_procargs_environment() {
    let mut buffer = Vec::new();
    let argc: libc::c_int = 2;
    buffer.extend_from_slice(&argc.to_ne_bytes());
    buffer.extend_from_slice(b"/bin/zsh\0");
    buffer.extend_from_slice(b"\0\0");
    buffer.extend_from_slice(b"zsh\0-l\0");
    buffer.extend_from_slice(b"RMUX_PANE=%1\0LANG=en_US.UTF-8\0\0");

    let environment = environment_from_macos_procargs(&buffer).expect("environment");

    assert_eq!(environment.get("RMUX_PANE").map(String::as_str), Some("%1"));
    assert_eq!(
        environment.get("LANG").map(String::as_str),
        Some("en_US.UTF-8")
    );
}
