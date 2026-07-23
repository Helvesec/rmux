use std::io;
use std::process::ChildStdout;

const READ_BUFFER_SIZE: usize = 8192;
const DISCARD_READ_BUDGET: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureProgress {
    Pending,
    Eof,
    LimitReached,
}

pub(super) struct StatusJobOutputCapture {
    stdout: ChildStdout,
    output: Vec<u8>,
    limit: usize,
    eof: bool,
}

impl StatusJobOutputCapture {
    pub(super) fn new(stdout: ChildStdout, limit: usize) -> Self {
        Self {
            stdout,
            output: Vec::new(),
            limit,
            eof: false,
        }
    }

    pub(super) fn poll(&mut self) -> io::Result<CaptureProgress> {
        if self.eof {
            return Ok(CaptureProgress::Eof);
        }

        let mut buffer = [0_u8; READ_BUFFER_SIZE];
        loop {
            let remaining = self.limit.saturating_sub(self.output.len());
            if remaining == 0 {
                return Ok(CaptureProgress::LimitReached);
            }
            let read_limit = remaining.min(buffer.len());
            match platform::read_available(&mut self.stdout, &mut buffer[..read_limit])? {
                platform::PipeRead::Pending => return Ok(CaptureProgress::Pending),
                platform::PipeRead::Eof => {
                    self.eof = true;
                    return Ok(CaptureProgress::Eof);
                }
                platform::PipeRead::Data(read) => {
                    self.output.extend_from_slice(&buffer[..read]);
                }
            }
        }
    }

    pub(super) fn poll_discard(&mut self) -> io::Result<CaptureProgress> {
        if self.eof {
            return Ok(CaptureProgress::Eof);
        }

        let mut buffer = [0_u8; READ_BUFFER_SIZE];
        for _ in 0..DISCARD_READ_BUDGET {
            match platform::read_available(&mut self.stdout, &mut buffer)? {
                platform::PipeRead::Pending => return Ok(CaptureProgress::Pending),
                platform::PipeRead::Eof => {
                    self.eof = true;
                    return Ok(CaptureProgress::Eof);
                }
                platform::PipeRead::Data(_) => {}
            }
        }
        Ok(CaptureProgress::Pending)
    }

    pub(super) fn into_output(self) -> Vec<u8> {
        self.output
    }
}

#[cfg(unix)]
mod platform {
    use std::io::{self, Read};
    use std::process::ChildStdout;

    use rustix::event::{poll, PollFd, PollFlags, Timespec};

    const NO_WAIT: Timespec = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    pub(super) enum PipeRead {
        Pending,
        Eof,
        Data(usize),
    }

    pub(super) fn read_available(
        stdout: &mut ChildStdout,
        buffer: &mut [u8],
    ) -> io::Result<PipeRead> {
        let mut fds = [PollFd::new(
            &*stdout,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        match poll(&mut fds, Some(&NO_WAIT)) {
            Ok(0) => return Ok(PipeRead::Pending),
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok(PipeRead::Pending),
            Err(error) => return Err(error.into()),
        }
        if !fds[0]
            .revents()
            .intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP)
        {
            return Ok(PipeRead::Pending);
        }

        loop {
            match stdout.read(buffer) {
                Ok(0) => return Ok(PipeRead::Eof),
                Ok(read) => return Ok(PipeRead::Data(read)),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(PipeRead::Pending);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io::{self, Read};
    use std::process::ChildStdout;

    use rmux_os::pipe::{child_stdout_readiness, ChildStdoutReadiness};

    pub(super) enum PipeRead {
        Pending,
        Eof,
        Data(usize),
    }

    pub(super) fn read_available(
        stdout: &mut ChildStdout,
        buffer: &mut [u8],
    ) -> io::Result<PipeRead> {
        let read_limit = match child_stdout_readiness(stdout)? {
            ChildStdoutReadiness::Pending => return Ok(PipeRead::Pending),
            ChildStdoutReadiness::Closed => return Ok(PipeRead::Eof),
            ChildStdoutReadiness::Bytes(available) => available.min(buffer.len()),
        };
        match stdout.read(&mut buffer[..read_limit]) {
            Ok(0) => Ok(PipeRead::Eof),
            Ok(read) => Ok(PipeRead::Data(read)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(PipeRead::Pending),
            Err(error) => Err(error),
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::io::{self, Read};
    use std::process::ChildStdout;

    pub(super) enum PipeRead {
        Pending,
        Eof,
        Data(usize),
    }

    pub(super) fn read_available(
        stdout: &mut ChildStdout,
        buffer: &mut [u8],
    ) -> io::Result<PipeRead> {
        match stdout.read(buffer) {
            Ok(0) => Ok(PipeRead::Eof),
            Ok(read) => Ok(PipeRead::Data(read)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(PipeRead::Pending),
            Err(error) => Err(error),
        }
    }
}
