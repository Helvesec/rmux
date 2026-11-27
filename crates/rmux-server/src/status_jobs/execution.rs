#[cfg(all(test, unix))]
use super::tests;
#[cfg(test)]
use super::STATUS_JOB_TIMEOUT;
use super::{
    CaptureProgress, StatusJobOutputCapture, STATUS_JOB_FORCE_TERMINATION_WAIT,
    STATUS_JOB_OUTPUT_DRAIN_GRACE, STATUS_JOB_OUTPUT_LIMIT, STATUS_JOB_POLL_INTERVAL,
    STATUS_JOB_TERMINATION_GRACE,
};
use crate::terminal::TerminalProfile;
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild, ProcessTreeController};
#[cfg(unix)]
use rustix::process::Signal;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
pub(super) fn run_status_job(command: &str, profile: Option<&TerminalProfile>) -> String {
    run_status_job_with_cancellation(command, profile, &AtomicBool::new(false))
}

#[cfg(all(test, windows))]
pub(super) fn run_status_job_with_timeout(
    command: &str,
    profile: Option<&TerminalProfile>,
    timeout: Duration,
) -> String {
    run_status_job_until(
        command,
        profile,
        &AtomicBool::new(false),
        timeout,
        &mut |_| {},
    )
}

#[cfg(test)]
pub(super) fn run_status_job_with_cancellation(
    command: &str,
    profile: Option<&TerminalProfile>,
    cancellation: &AtomicBool,
) -> String {
    run_status_job_until(
        command,
        profile,
        cancellation,
        STATUS_JOB_TIMEOUT,
        &mut |_| {},
    )
}

pub(super) fn run_status_job_until(
    command: &str,
    profile: Option<&TerminalProfile>,
    cancellation: &AtomicBool,
    timeout: Duration,
    publish: &mut dyn FnMut(String),
) -> String {
    if cancellation.load(Ordering::Acquire) {
        return String::new();
    }
    let mut process = status_job_command(command, profile);
    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) =
        ProcessTreeChild::spawn_with_console_window(&mut process, ConsoleWindowBehavior::Suppress)
    else {
        return String::new();
    };
    let process_group = child.controller();
    let Some(stdout) = child.child_mut().stdout.take() else {
        terminate_status_job(child, &process_group);
        return String::new();
    };
    let mut stdout = StatusJobOutputCapture::new(stdout, STATUS_JOB_OUTPUT_LIMIT);

    let started = Instant::now();
    let mut published_bytes = 0;
    loop {
        if cancellation.load(Ordering::Acquire) {
            terminate_status_job(child, &process_group);
            wait_for_status_job_output_close(&mut stdout);
            return String::new();
        }

        match stdout.poll() {
            Ok(CaptureProgress::Pending | CaptureProgress::Eof) => {}
            Ok(CaptureProgress::LimitReached) => {
                terminate_status_job(child, &process_group);
                wait_for_status_job_output_close(&mut stdout);
                return status_job_stdout(stdout.into_output());
            }
            Err(_) => {
                terminate_status_job(child, &process_group);
                return String::new();
            }
        }

        if let Some((consumed, line)) = stdout.last_complete_line(published_bytes) {
            published_bytes = consumed;
            publish(line);
        }

        // A timeout requires a running-child observation made after the deadline.
        // Sampling elapsed time later could pair it with a stale false exit check
        // if this observer pauses while the child completes before the deadline.
        let deadline_expired = started.elapsed() >= timeout;
        let child_exited = match child.has_exited() {
            Ok(exited) => exited,
            Err(_) => {
                terminate_status_job(child, &process_group);
                wait_for_status_job_output_close(&mut stdout);
                return String::new();
            }
        };
        #[cfg(all(test, unix))]
        tests::after_child_observation(&mut child, child_exited, started);
        if child_exited {
            terminate_completed_status_job(child, &process_group);
            return status_job_stdout(drain_completed_status_job_output(stdout));
        }

        if deadline_expired {
            let published = stdout
                .last_complete_line(0)
                .map(|(_, line)| line)
                .unwrap_or_default();
            terminate_status_job(child, &process_group);
            wait_for_status_job_output_close(&mut stdout);
            return published;
        }

        thread::sleep(STATUS_JOB_POLL_INTERVAL);
    }
}

fn drain_completed_status_job_output(mut stdout: StatusJobOutputCapture) -> Vec<u8> {
    let deadline = Instant::now() + STATUS_JOB_OUTPUT_DRAIN_GRACE;
    loop {
        match stdout.poll() {
            Ok(CaptureProgress::Eof) | Err(_) => {
                return stdout.into_output();
            }
            Ok(CaptureProgress::LimitReached) => {
                wait_for_status_job_output_close_until(&mut stdout, deadline);
                return stdout.into_output();
            }
            Ok(CaptureProgress::Pending) if Instant::now() >= deadline => {
                return stdout.into_output();
            }
            Ok(CaptureProgress::Pending) => thread::sleep(STATUS_JOB_POLL_INTERVAL),
        }
    }
}

fn wait_for_status_job_output_close(stdout: &mut StatusJobOutputCapture) {
    wait_for_status_job_output_close_until(stdout, Instant::now() + STATUS_JOB_OUTPUT_DRAIN_GRACE);
}

fn wait_for_status_job_output_close_until(stdout: &mut StatusJobOutputCapture, deadline: Instant) {
    loop {
        match stdout.poll_discard() {
            Ok(CaptureProgress::Eof) | Err(_) => return,
            Ok(CaptureProgress::Pending) if Instant::now() >= deadline => return,
            Ok(CaptureProgress::Pending) => thread::sleep(STATUS_JOB_POLL_INTERVAL),
            Ok(CaptureProgress::LimitReached) => {
                unreachable!("discarding output does not enforce the capture limit")
            }
        }
    }
}

#[cfg(unix)]
fn request_status_job_stop(child: &mut ProcessTreeChild) {
    let _ = child.forward_signal(Signal::TERM.as_raw());
}

#[cfg(not(unix))]
fn request_status_job_stop(_: &mut ProcessTreeChild) {}

fn terminate_status_job(mut child: ProcessTreeChild, process_group: &ProcessTreeController) {
    request_status_job_stop(&mut child);
    thread::sleep(STATUS_JOB_TERMINATION_GRACE);
    terminate_status_job_immediately(child, process_group);
}

fn terminate_completed_status_job(child: ProcessTreeChild, process_group: &ProcessTreeController) {
    terminate_status_job_immediately(child, process_group);
}

fn terminate_status_job_immediately(
    mut child: ProcessTreeChild,
    process_group: &ProcessTreeController,
) {
    // `wait` disarms both the Unix group identity and Windows kill-on-close.
    // Terminate first while the unreaped leader still makes the PGID
    // non-reusable and the Job Object still owns every descendant.
    let tree_stopped = process_group
        .terminate_and_wait(STATUS_JOB_FORCE_TERMINATION_WAIT)
        .unwrap_or(false);
    if !tree_stopped {
        // Enumeration is unavailable on a few Unix targets and can fail under
        // restrictive host policies. Preserve the bounded cleanup contract by
        // making one final force-termination attempt before reaping the leader.
        let _ = child.terminate();
    }
    let _ = child.wait();
}

fn status_job_command(command: &str, profile: Option<&TerminalProfile>) -> Command {
    if let Some(profile) = profile {
        let mut process = status_job_profile_command(command, profile);
        process.env_clear();
        for (name, value) in profile.raw_environment() {
            process.env(name, value);
        }
        return process;
    }

    shell_command(command)
}

#[cfg(unix)]
fn status_job_profile_command(command: &str, profile: &TerminalProfile) -> Command {
    let mut process =
        crate::terminal::shell_std_command(std::path::Path::new("/bin/sh"), profile.cwd(), command);
    process.current_dir(profile.cwd());
    process
}

#[cfg(not(unix))]
fn status_job_profile_command(command: &str, profile: &TerminalProfile) -> Command {
    profile.shell_std_command(command)
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let mut process = Command::new(shell);
        process.arg("/D").arg("/S").arg("/C");
        process.raw_arg(rmux_os::command::cmd_c_verbatim_tail(command));
        process
    }

    #[cfg(not(windows))]
    {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let mut process = Command::new(shell);
        process.arg("-c").arg(command);
        process
    }
}

fn status_job_stdout(stdout: Vec<u8>) -> String {
    let bytes = stdout.strip_suffix(b"\n").unwrap_or(&stdout);
    let line = bytes
        .rsplit(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned()
}
