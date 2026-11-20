use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::time::{Duration, Instant};

use rmux_core::{EnvironmentStore, OptionStore, PaneGeometry};
use rmux_proto::{OptionName, ProcessCommand, ScopeSelector, SetOptionMode};

use crate::pane_terminal_process::{open_pane_terminal, PaneTerminal};
use crate::terminal::TerminalProfile;

const DEADLINE: Duration = Duration::from_secs(5);

fn pane(command: ProcessCommand) -> Result<PaneTerminal, rmux_proto::RmuxError> {
    let mut environment = EnvironmentStore::default();
    for name in ["HISTFILE", "INPUTRC", "BASH_ENV", "ENV"] {
        environment.set(
            ScopeSelector::Global,
            name.to_owned(),
            "/dev/null".to_owned(),
        );
    }
    let mut options = OptionStore::default();
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/sh".to_owned(),
            SetOptionMode::Replace,
        )
        .unwrap();
    let profile = TerminalProfile::for_run_shell_with_base_environment(
        &environment,
        &options,
        None,
        None,
        Path::new("/unused-post-exec.sock"),
        None,
        false,
        None,
        Some(Path::new("/")),
    )
    .unwrap();
    open_pane_terminal(
        &rmux_pty::ProcessShell,
        PaneGeometry::new(0, 0, 80, 24),
        profile,
        None,
        Some(&command),
    )
}

fn read_until(io: &rmux_pty::PtyIo, marker: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + DEADLINE;
    let mut output = Vec::new();
    while Instant::now() < deadline {
        let mut bytes = [0; 1024];
        match io.try_read(&mut bytes) {
            Ok(count) => output.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            other => panic!("PTY read: {other:?}"),
        }
        if output.windows(marker.len()).any(|part| part == marker) {
            return output;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "missing {marker:?} in {:?}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn post_exec_probe_preserves_foreground_job_control() {
    assert!(
        pane(ProcessCommand::Argv(vec![
            "/nonexistent-rmux-post-exec-program".to_owned()
        ]))
        .is_err(),
        "failed exec must remain a spawn error"
    );
    interactive_job_control();
    for command in [
        ProcessCommand::Argv(vec!["/bin/cat".to_owned()]),
        ProcessCommand::Shell("exec /bin/cat".to_owned()),
    ] {
        check_foreground_cat(pane(command).unwrap());
    }
    // Direct argv executes cat without a shell repairing its inherited state.
    // Successful spawn includes EOF on Command's close-on-exec status channel.
    let terminal = rmux_pty::test_support::with_blocked_interrupt(|| {
        pane(ProcessCommand::Argv(vec!["/bin/cat".to_owned()])).unwrap()
    })
    .unwrap();
    check_foreground_cat(terminal);
}

fn check_foreground_cat(mut terminal: PaneTerminal) {
    assert!(
        terminal.is_alive().unwrap(),
        "exec success is not process exit"
    );
    assert!(terminal.exit_status().unwrap().is_none());
    let master = terminal.clone_master_for_output_reader().unwrap();
    let io = master.io();
    assert_eq!(
        rmux_os::process::unix::foreground_pid(io.as_fd()),
        Some(terminal.pid()),
        "the executed job must own its terminal foreground group"
    );
    io.write_all(b"post-exec-input\n").unwrap();
    // The first line is terminal echo; the second proves the job read stdin.
    read_until(io, b"post-exec-input\r\npost-exec-input\r\n");
    assert!(terminal.is_alive().unwrap());
    assert!(!terminal.continue_if_stopped().unwrap());
    io.write_all(&[3]).unwrap();
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        drain_exit_output(io);
        if let Some(status) = terminal.exit_status().unwrap() {
            assert_eq!(status.signal(), Some(rustix::process::Signal::INT.as_raw()));
            assert!(!terminal.is_alive().unwrap());
            assert_eq!(terminal.exit_status().unwrap(), Some(status));
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the foreground job ignored terminal SIGINT after successful exec");
}

fn drain_exit_output(io: &rmux_pty::PtyIo) {
    // Model the server's active output reader: macOS may hold the last slave
    // close until the master drains the job's final output (bash prints exit).
    let mut output = [0; 4096];
    let _ = io.try_read(&mut output);
}

fn wait_for_foreground(io: &rmux_pty::PtyIo, expected: impl Fn(u32) -> bool) -> u32 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(pid) = rmux_os::process::unix::foreground_pid(io.as_fd()) {
            if expected(pid) {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "foreground group did not transfer"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn interactive_job_control() {
    let mut terminal = pane(ProcessCommand::Argv(
        ["/bin/bash", "--noprofile", "--norc", "-i"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    ))
    .unwrap();
    let master = terminal.clone_master_for_output_reader().unwrap();
    let io = master.io();
    io.write_all(b"PS1=$(printf 'JOB_%s> ' PROMPT); stty -echo\n")
        .unwrap();
    read_until(io, b"JOB_PROMPT> ");
    // macOS cat exits on EINTR after stop/continue. A shell read loop retries
    // interrupted reads so the fixture measures job control, not cat's policy.
    io.write_all(b"/bin/sh -c 'while :; do IFS= read -r line; code=$?; if [ \"$code\" -eq 0 ]; then printf \"%s\\n\" \"$line\"; elif [ \"$code\" -eq 1 ]; then exit 0; fi; done'\n").unwrap();
    let job = wait_for_foreground(io, |pid| pid != terminal.pid());
    assert!(terminal.is_alive().unwrap());
    assert!(!terminal.continue_if_stopped().unwrap());
    io.write_all(b"job-input\n").unwrap();
    read_until(io, b"job-input\r\n");
    io.write_all(&[26]).unwrap();
    wait_for_foreground(io, |pid| pid == terminal.pid());
    read_until(io, b"JOB_PROMPT> ");
    assert!(
        terminal.is_alive().unwrap(),
        "stopped job is not shell exit"
    );
    assert!(!terminal.continue_if_stopped().unwrap());
    io.write_all(b"fg\n").unwrap();
    wait_for_foreground(io, |pid| pid == job);
    io.write_all(b"resumed-input\n").unwrap();
    read_until(io, b"resumed-input\r\n");
    io.write_all(&[3]).unwrap();
    wait_for_foreground(io, |pid| pid == terminal.pid());
    read_until(io, b"JOB_PROMPT> ");
    assert!(
        terminal.is_alive().unwrap(),
        "Ctrl-C must leave the shell alive"
    );
    io.write_all(b"exit 0\n").unwrap();
    let deadline = Instant::now() + DEADLINE;
    loop {
        drain_exit_output(io);
        if let Some(status) = terminal.exit_status().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "interactive shell did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
}
