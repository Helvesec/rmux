use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rmux_pty::{PtyPair, TerminalSize};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::process::{kill_process, Pid, Signal};
use rustix::termios::{
    tcgetattr, tcsetattr, tcsetwinsize, LocalModes, OptionalActions, SpecialCodeIndex, Termios,
    Winsize,
};

use crate::common::{terminate_child, CliHarness};

const POLL_SLICE: Duration = Duration::from_millis(50);

pub(crate) struct AttachedSession {
    master: File,
    terminal: File,
    original_termios: Termios,
    child: Child,
}

impl AttachedSession {
    pub(crate) fn spawn(
        harness: &CliHarness,
        session_name: &str,
        size: TerminalSize,
    ) -> Result<Self, Box<dyn Error>> {
        Self::spawn_configured(harness, session_name, size, |_| {})
    }

    pub(crate) fn spawn_command(
        harness: &CliHarness,
        args: &[&str],
        size: TerminalSize,
    ) -> Result<Self, Box<dyn Error>> {
        Self::spawn_command_configured(harness, args, size, |_| {})
    }

    pub(crate) fn spawn_with_env(
        harness: &CliHarness,
        session_name: &str,
        size: TerminalSize,
        environment: &[(&str, &str)],
    ) -> Result<Self, Box<dyn Error>> {
        Self::spawn_configured(harness, session_name, size, |attach| {
            for (name, value) in environment {
                attach.env(name, value);
            }
        })
    }

    fn spawn_configured<F>(
        harness: &CliHarness,
        session_name: &str,
        size: TerminalSize,
        configure: F,
    ) -> Result<Self, Box<dyn Error>>
    where
        F: FnOnce(&mut Command),
    {
        let args = ["attach-session", "-t", session_name];
        Self::spawn_command_configured(harness, &args, size, configure)
    }

    fn spawn_command_configured<F>(
        harness: &CliHarness,
        args: &[&str],
        size: TerminalSize,
        configure: F,
    ) -> Result<Self, Box<dyn Error>>
    where
        F: FnOnce(&mut Command),
    {
        let pty = PtyPair::open_with_size(size)?;
        let master = File::from(pty.master().try_clone()?.into_owned_fd()?);
        let terminal = File::from(pty.slave().try_clone()?.into_owned_fd());
        let original_termios = prepare_canonical_termios(&terminal)?;

        let mut attach = harness.base_command();
        attach
            .args(args)
            .stdin(Stdio::from(pty.slave().try_clone()?.into_owned_fd()))
            .stdout(Stdio::from(pty.slave().try_clone()?.into_owned_fd()))
            .stderr(Stdio::from(pty.slave().try_clone()?.into_owned_fd()));
        configure(&mut attach);
        drop(pty);

        let child = attach.spawn()?;

        Ok(Self {
            master,
            terminal,
            original_termios,
            child,
        })
    }

    pub(crate) fn master_mut(&mut self) -> &mut File {
        &mut self.master
    }

    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn wait_for_raw_mode(&self, timeout: Duration) -> Result<(), Box<dyn Error>> {
        wait_for_raw_mode(&self.terminal, timeout)
    }

    pub(crate) fn wait_for_exit(
        &mut self,
        timeout: Duration,
    ) -> Result<ExitStatus, Box<dyn Error>> {
        wait_for_exit(&mut self.child, &mut self.master, timeout)
    }

    pub(crate) fn wait_for_exit_with_output(
        &mut self,
        timeout: Duration,
    ) -> Result<(ExitStatus, Vec<u8>), Box<dyn Error>> {
        wait_for_exit_with_output(&mut self.child, &mut self.master, timeout)
    }

    pub(crate) fn assert_restored(&self) -> Result<(), Box<dyn Error>> {
        assert_termios_eq(&self.original_termios, &tcgetattr(&self.terminal)?);
        Ok(())
    }

    pub(crate) fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
        self.master.write_all(bytes)?;
        self.master.flush()?;
        Ok(())
    }

    pub(crate) fn resize(&mut self, size: TerminalSize) -> Result<(), Box<dyn Error>> {
        tcsetwinsize(
            &self.terminal,
            Winsize {
                ws_row: size.rows,
                ws_col: size.cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        let pid = Pid::from_raw(
            i32::try_from(self.child.id())
                .map_err(|_| "attach-session child pid does not fit in i32")?,
        )
        .ok_or("attach-session child pid must be positive")?;
        kill_process(pid, Signal::WINCH)?;
        Ok(())
    }
}

impl Drop for AttachedSession {
    fn drop(&mut self) {
        let _ = terminate_child(&mut self.child);
    }
}

pub(crate) fn prepare_canonical_termios<Fd>(fd: &Fd) -> Result<Termios, Box<dyn Error>>
where
    Fd: std::os::fd::AsFd,
{
    let mut termios = tcgetattr(fd)?;
    termios.local_modes |= LocalModes::ICANON | LocalModes::ECHO;
    termios.special_codes[SpecialCodeIndex::VMIN] = 4;
    termios.special_codes[SpecialCodeIndex::VTIME] = 9;
    tcsetattr(fd, OptionalActions::Now, &termios)?;
    Ok(tcgetattr(fd)?)
}

fn wait_for_raw_mode<Fd>(fd: &Fd, timeout: Duration) -> Result<(), Box<dyn Error>>
where
    Fd: std::os::fd::AsFd,
{
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        let termios = tcgetattr(fd)?;
        if !termios.local_modes.contains(LocalModes::ICANON)
            && !termios.local_modes.contains(LocalModes::ECHO)
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let final_termios = tcgetattr(fd)?;
    Err(format!(
        "attach-session never entered raw mode (ICANON={}, ECHO={})",
        final_termios.local_modes.contains(LocalModes::ICANON),
        final_termios.local_modes.contains(LocalModes::ECHO),
    )
    .into())
}

fn wait_for_exit(
    child: &mut Child,
    reader: &mut File,
    timeout: Duration,
) -> Result<ExitStatus, Box<dyn Error>> {
    wait_for_exit_with_output(child, reader, timeout).map(|(status, _)| status)
}

fn wait_for_exit_with_output(
    child: &mut Child,
    reader: &mut File,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();

    loop {
        if let Some(status) = child.try_wait()? {
            drain_attach_output_into(reader, &mut output)?;
            return Ok((status, output));
        }

        drain_attach_output_into(reader, &mut output)?;

        if Instant::now() >= deadline {
            return Err("attach-session did not exit after detach-client".into());
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn drain_attach_output(reader: &mut File) -> Result<(), Box<dyn Error>> {
    let mut sink = Vec::new();
    drain_attach_output_into(reader, &mut sink)
}

pub(crate) fn drain_attach_output_bytes(reader: &mut File) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut output = Vec::new();
    drain_attach_output_into(reader, &mut output)?;
    Ok(output)
}

fn drain_attach_output_into(reader: &mut File, output: &mut Vec<u8>) -> Result<(), Box<dyn Error>> {
    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut bytes = [0_u8; 4096];

    loop {
        let mut fds = [PollFd::new(
            &*reader,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        if poll(&mut fds, Some(&timeout))? == 0 {
            return Ok(());
        }

        let count = reader.read(&mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        output.extend_from_slice(&bytes[..count]);
    }
}

pub(crate) fn read_until_contains(
    reader: &mut File,
    needle: &str,
    timeout: Duration,
) -> Result<String, Box<dyn Error>> {
    read_until_contains_all(reader, &[needle], timeout)
}

pub(crate) fn read_until_contains_all(
    reader: &mut File,
    needles: &[&str],
    timeout: Duration,
) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut buffer = String::new();
    let mut bytes = [0_u8; 4096];

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let poll_timeout = remaining.min(POLL_SLICE);
        let timeout = Timespec {
            tv_sec: poll_timeout.as_secs() as i64,
            tv_nsec: poll_timeout.subsec_nanos().into(),
        };
        let mut fds = [PollFd::new(
            reader,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];

        if poll(&mut fds, Some(&timeout))? == 0 {
            continue;
        }

        let count = reader.read(&mut bytes)?;
        if count == 0 {
            break;
        }

        buffer.push_str(&String::from_utf8_lossy(&bytes[..count]));
        if needles.iter().all(|needle| buffer.contains(needle)) {
            return Ok(buffer);
        }
    }

    Err(format!(
        "timed out waiting for attach output containing {:?}: {buffer:?}",
        needles
    )
    .into())
}

pub(crate) fn assert_termios_eq(expected: &Termios, actual: &Termios) {
    assert_eq!(actual.input_modes, expected.input_modes);
    assert_eq!(actual.output_modes, expected.output_modes);
    assert_eq!(actual.control_modes, expected.control_modes);
    assert_eq!(
        comparable_local_modes(actual.local_modes),
        comparable_local_modes(expected.local_modes)
    );
    #[cfg(target_os = "linux")]
    assert_eq!(actual.line_discipline, expected.line_discipline);
    assert_eq!(
        format!("{:?}", actual.special_codes),
        format!("{:?}", expected.special_codes)
    );
    assert_eq!(actual.input_speed(), expected.input_speed());
    assert_eq!(actual.output_speed(), expected.output_speed());
}

#[cfg(target_os = "macos")]
fn comparable_local_modes(mut modes: LocalModes) -> LocalModes {
    modes.remove(LocalModes::PENDIN);

    modes
}

#[cfg(not(target_os = "macos"))]
fn comparable_local_modes(modes: LocalModes) -> LocalModes {
    modes
}
