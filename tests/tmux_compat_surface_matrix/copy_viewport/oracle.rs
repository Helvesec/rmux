use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rustix::event::{PollFd, PollFlags};
use serde_json::{json, Value};

const PREREQUISITE: &str = "requires Node and pinned xterm 6.0.0: run npm --prefix tests/xterm-oracle ci --ignore-scripts (RMUX_XTERM_NODE selects Node)";
const MAX_REPLY: usize = 32768;

/// One owned persistent xterm; nonblocking pipes, one bounded request in flight.
/// A reply acknowledges an exact byte offset, never merely a recent row sample.
pub(super) struct Oracle {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    request: Vec<u8>,
    written: usize,
    response: Vec<u8>,
    response_drained: bool,
    sequence: u64,
    sent: usize,
    acknowledged: Option<usize>,
    resetting: bool,
    stopped: bool,
    pub(super) rows: Vec<String>,
}

impl Oracle {
    pub(super) fn new() -> io::Result<Self> {
        let node = std::env::var_os("RMUX_XTERM_NODE").unwrap_or_else(|| "node".into());
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/xterm-oracle/viewport-oracle.mjs");
        Self::spawn(Command::new(node).arg(script))
    }

    pub(super) fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut child = command
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| io::Error::new(error.kind(), format!("{PREREQUISITE}: {error}")))?;
        let input = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let oracle = Self {
            child,
            input,
            output,
            request: Vec::new(),
            written: 0,
            response: Vec::new(),
            response_drained: false,
            sequence: 0,
            sent: 0,
            acknowledged: None,
            resetting: false,
            stopped: false,
            rows: Vec::new(),
        };
        for fd in [oracle.input.as_raw_fd(), oracle.output.as_raw_fd()] {
            // SAFETY: these are live owned pipe descriptors, not shared PTYs.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags == -1
                || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
            {
                return Err(io::Error::last_os_error()); // Drop kills and reaps.
            }
        }
        Ok(oracle)
    }

    pub(super) fn current(&self, bytes: usize) -> bool {
        !self.stopped
            && !self.resetting
            && self.request.is_empty()
            && self.response.is_empty()
            && self.response_drained
            && self.acknowledged == Some(bytes)
    }

    pub(super) fn needs_service(&self, bytes: usize) -> bool {
        !self.response_drained || (self.request.is_empty() && self.needs_request(bytes))
    }

    fn needs_request(&self, bytes: usize) -> bool {
        self.resetting || self.acknowledged != Some(bytes)
    }

    pub(super) fn service(&mut self, bytes: &[u8]) -> io::Result<()> {
        let result = self.exchange(bytes);
        if result.is_err() {
            self.stop();
        }
        result
    }

    fn exchange(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.response_drained = false;
        self.check_child()?;
        self.read_response()?;
        // Do not let a new request legitimize residue from the previous ACK.
        if self.response_drained && self.request.is_empty() && self.needs_request(bytes.len()) {
            let end = (self.sent + 8192).min(bytes.len());
            self.sequence += 1;
            self.request = serde_json::to_vec(&json!({
                "sequence": self.sequence, "end": end, "reset": self.resetting,
                "bytes": &bytes[self.sent..end],
            }))?;
            self.request.push(b'\n');
            self.written = 0;
            self.sent = end;
        }
        if self.written < self.request.len() {
            match self.input.write(&self.request[self.written..]) {
                Ok(0) => return Err(invalid("oracle stdin closed")),
                Ok(n) => self.written += n,
                Err(e) if transient(&e) => {}
                Err(e) => return Err(io::Error::new(e.kind(), format!("{PREREQUISITE}: {e}"))),
            }
        }
        self.check_child()
    }

    fn check_child(&mut self) -> io::Result<()> {
        if self.stopped {
            return Err(invalid("oracle already stopped"));
        }
        if let Some(status) = self.child.try_wait()? {
            self.stopped = true; // The owning handle has already reaped it.
            return Err(invalid(&format!("oracle exited {status}; {PREREQUISITE}")));
        }
        Ok(())
    }

    fn read_response(&mut self) -> io::Result<()> {
        if self.request.is_empty() && !self.response.is_empty() {
            return Err(invalid("unsolicited oracle response"));
        }
        // Exhaustion is NOT WouldBlock. Continue on the next fair service turn,
        // even if there is no new poll event after an exactly boundary-sized ACK.
        // A short ACK still gets an immediate nonblocking boundary check.
        let mut buffer = [0; 8192];
        let mut remaining = buffer.len();
        while remaining > 0 {
            match self.output.read(&mut buffer[..remaining]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("xterm oracle EOF; {PREREQUISITE}"),
                    ))
                }
                Ok(n) => {
                    remaining -= n;
                    if self.request.is_empty() {
                        return Err(invalid("unsolicited oracle response"));
                    }
                    if self.response.len() + n > MAX_REPLY {
                        return Err(invalid("oracle reply limit"));
                    }
                    self.response.extend_from_slice(&buffer[..n]);
                    if let Some(end) = self.response.iter().position(|b| *b == b'\n') {
                        if end + 1 != self.response.len() {
                            return Err(invalid("extra oracle response"));
                        }
                        self.accept()?;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.response_drained = true;
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn accept(&mut self) -> io::Result<()> {
        let response: Value = serde_json::from_slice(&self.response).map_err(io::Error::other)?;
        if let Some(error) = response.get("error") {
            return Err(invalid(&error.to_string()));
        }
        if self.request.is_empty()
            || self.written != self.request.len()
            || response["protocol"] != 1
            || response["xterm"] != "6.0.0"
            || response["sequence"].as_u64() != Some(self.sequence)
            || response["end"].as_u64() != Some(self.sent as u64)
        {
            return Err(invalid("oracle protocol/acknowledgement mismatch"));
        }
        let rows: Vec<String> =
            serde_json::from_value(response["rows"].clone()).map_err(io::Error::other)?;
        if rows.len() != 24 {
            return Err(invalid("oracle must return all 24 rows"));
        }
        self.rows = rows;
        self.acknowledged = Some(self.sent);
        self.resetting = false;
        self.request.clear();
        self.response.clear();
        Ok(())
    }

    pub(super) fn poll_fds(&self) -> [PollFd<'_>; 2] {
        [
            PollFd::new(&self.output, PollFlags::IN),
            PollFd::new(
                &self.input,
                if self.written < self.request.len() {
                    PollFlags::OUT
                } else {
                    PollFlags::empty()
                },
            ),
        ]
    }

    #[cfg(test)]
    pub(super) fn reset(&mut self) {
        assert!(self.request.is_empty());
        self.sent = 0;
        self.acknowledged = None;
        self.resetting = true;
        self.rows.clear();
    }

    pub(super) fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            // Normal owned cancellation: no detached workers or reader threads.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("xterm viewport oracle: {message}"),
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod boundary;
