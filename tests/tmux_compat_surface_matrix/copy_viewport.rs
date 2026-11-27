use std::fs::File;
use std::io::{self, Read};
use std::time::{Duration, Instant};

use rmux_core::{input::InputParser, Screen};
use rmux_proto::TerminalSize;
use rustix::event::{poll, PollFd, PollFlags, Timespec};

mod oracle;

/// Text observation for the 80x24 numbered-line fixture. This does not assert
/// cursor, style, graphics, or general terminal parity. Xterm owns visible text.
pub(super) struct CopyViewport {
    oracle: oracle::Oracle,
    // Correlated auxiliary state can ONLY veto an incomplete sequence. Neither
    // its cells nor its text ever supply readiness or compared rows (R1).
    parser: InputParser,
    screen: Screen,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ViewportPhase {
    Live,
    CopyBottom,
    CopyTop,
}

impl CopyViewport {
    pub(super) fn new() -> io::Result<Self> {
        Ok(Self {
            oracle: oracle::Oracle::new()?,
            parser: InputParser::new(),
            screen: Screen::new(TerminalSize { cols: 80, rows: 24 }, 0),
            bytes: Vec::new(),
        })
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > 8192 || self.bytes.len() + bytes.len() > 1_048_576 {
            return Err(io::Error::other("copy viewport input limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        self.parser.parse(bytes, &mut self.screen);
        Ok(())
    }

    pub(super) fn rows(&self) -> Vec<String> {
        self.oracle.rows.clone()
    }

    pub(super) fn ready(&self, phase: ViewportPhase) -> bool {
        self.oracle.current(self.bytes.len())
            && self.parser.pending_bytes().is_empty()
            && phase.matches(&self.oracle.rows)
    }
}

impl ViewportPhase {
    fn matches(self, rows: &[String]) -> bool {
        if rows.len() != 24 || !rows[23].starts_with("[alpha] 0:bash*") {
            return false;
        }
        match self {
            Self::Live | Self::CopyBottom => {
                let first = match self {
                    Self::Live => rows[0] == "P0-LINE-09",
                    Self::CopyBottom => {
                        rows[0].starts_with("P0-LINE-09 ")
                            && copy_position(&rows[0])
                                .is_some_and(|(offset, total)| offset == 0 && total > 0)
                    }
                    Self::CopyTop => unreachable!(),
                };
                first
                    && (1..22).all(|row| rows[row] == format!("P0-LINE-{:02}", row + 9))
                    && rows[22] == "tester@RMUXHOST:~$"
            }
            Self::CopyTop => {
                let Some(first) = rows.iter().position(|row| row == "P0-LINE-01") else {
                    return false;
                };
                // The fixture has 30 numbered lines and a final prompt in a
                // 23-row pane: history = prelude + 31 - 23. At history top,
                // `first` is that prelude's height (5 for the macOS bash
                // banner). Bind it to the header instead of assuming <= 3.
                // Keep a visible body below the header and require every
                // consecutive numbered row through the last pane row.
                (1..23).contains(&first)
                    && copy_position(&rows[0])
                        .is_some_and(|(offset, total)| offset == total && total == first + 8)
                    && (first..23).all(|row| rows[row] == format!("P0-LINE-{:02}", row - first + 1))
            }
        }
    }
}

fn copy_position(row: &str) -> Option<(usize, usize)> {
    let (_, position) = row.rsplit_once('[')?;
    let (offset, total) = position.strip_suffix(']')?.split_once('/')?;
    Some((offset.parse().ok()?, total.parse().ok()?))
}

pub(super) fn comparable_rows(viewport: &CopyViewport) -> Vec<String> {
    let mut rows = viewport.rows();
    if rows.len() == 24 {
        // Bind normalization to the actual fixture fields, not arbitrary
        // clock-shaped text anywhere in the header or status.
        if copy_position(&rows[0]).is_some() {
            if let Some((prefix, _)) = rows[0].rsplit_once(" [") {
                if prefix.trim_start().len() == 5 {
                    let start = prefix.len() - 5;
                    normalize_clock(&mut rows[0], start);
                }
            }
        }
        if rows[23].starts_with("[alpha] 0:bash*") {
            if let Some((prefix, suffix)) = rows[23].rsplit_once("\" ") {
                // Default tmux status: quoted hostname, HH:MM, DD-Mon-YY.
                if suffix.len() == 15 && suffix.as_bytes()[5] == b' ' {
                    let start = prefix.len() + 2;
                    normalize_clock(&mut rows[23], start);
                }
            }
        }
    }
    rows
}

fn normalize_clock(row: &mut String, start: usize) {
    let clock = &row.as_bytes()[start..start + 5];
    if clock[2] == b':' && [0, 1, 3, 4].into_iter().all(|i| clock[i].is_ascii_digit()) {
        row.replace_range(start..start + 5, "HH:MM");
    }
}

/// Service both nonblocking PTYs until the fixture transition is visible on
/// both and every text row matches native tmux. WouldBlock/quietness is never
/// a completion condition. The deadline is fixed, even under continuous output.
/// Acceptance observes drained response channels and live children during this
/// turn. It is not atomic across processes and cannot predict later data/exits.
pub(super) fn wait_for_viewports(
    rmux: &mut File,
    tmux: &mut File,
    observed: &mut [CopyViewport; 2],
    phase: ViewportPhase,
    timeout: Duration,
) -> io::Result<()> {
    let result = observe(rmux, tmux, observed, phase, timeout);
    if result.is_err() {
        for viewport in observed {
            viewport.oracle.stop();
        }
    }
    result
}

fn observe(
    rmux: &mut File,
    tmux: &mut File,
    observed: &mut [CopyViewport; 2],
    phase: ViewportPhase,
    timeout: Duration,
) -> io::Result<()> {
    let end = Instant::now() + timeout;
    let mut buffer = [0; 8192];
    loop {
        end.checked_duration_since(Instant::now()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "incomplete {phase:?} viewport: rmux {} bytes {:?}; tmux {} bytes {:?}",
                    observed[0].bytes.len(),
                    observed[0].rows(),
                    observed[1].bytes.len(),
                    observed[1].rows(),
                ),
            )
        })?;
        // One read per client per iteration preserves fairness and the bound.
        let rmux_drained = read_chunk(rmux, &mut observed[0], &mut buffer)?;
        let tmux_drained = read_chunk(tmux, &mut observed[1], &mut buffer)?;
        for viewport in observed.iter_mut() {
            viewport.oracle.service(&viewport.bytes)?;
        }
        if rmux_drained
            && tmux_drained
            && observed.iter().all(|viewport| viewport.ready(phase))
            && comparable_rows(&observed[0]) == comparable_rows(&observed[1])
        {
            eprintln!(
                "completed {phase:?} viewport matches tmux: {:?}",
                comparable_rows(&observed[1])
            );
            return Ok(());
        }
        // After an acknowledgement there may already be more buffered bytes.
        // Submit those on the next fair turn instead of waiting for new input.
        if !rmux_drained
            || !tmux_drained
            || observed
                .iter()
                .any(|v| v.oracle.needs_service(v.bytes.len()))
        {
            continue;
        }
        let [rmux_out, rmux_in] = observed[0].oracle.poll_fds();
        let [tmux_out, tmux_in] = observed[1].oracle.poll_fds();
        let mut fds = [
            PollFd::new(&*rmux, PollFlags::IN),
            PollFd::new(&*tmux, PollFlags::IN),
            rmux_out,
            rmux_in,
            tmux_out,
            tmux_in,
        ];
        let remaining = end.saturating_duration_since(Instant::now());
        let timeout = Timespec::try_from(remaining).map_err(io::Error::other)?;
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn read_chunk(file: &mut File, viewport: &mut CopyViewport, buffer: &mut [u8]) -> io::Result<bool> {
    match file.read(buffer) {
        Ok(0) => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "attach PTY closed",
        )),
        Ok(count) => {
            viewport.feed(&buffer[..count])?;
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(error.kind() == io::ErrorKind::WouldBlock)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
