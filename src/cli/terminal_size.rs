use std::os::fd::AsRawFd;

use rmux_proto::TerminalSize;

const DEFAULT_SESSION_COLS: u16 = 80;
const DEFAULT_SESSION_ROWS: u16 = 24;

pub(super) fn build_terminal_size(cols: Option<u16>, rows: Option<u16>) -> Option<TerminalSize> {
    match (cols, rows) {
        (None, None) => None,
        (cols, rows) => Some(TerminalSize {
            cols: cols.unwrap_or(DEFAULT_SESSION_COLS),
            rows: rows.unwrap_or(DEFAULT_SESSION_ROWS),
        }),
    }
}

pub(super) fn current_terminal_size() -> Option<TerminalSize> {
    terminal_size_from_fd(&std::io::stdin()).or_else(|| terminal_size_from_fd(&std::io::stdout()))
}

fn terminal_size_from_fd<Fd>(fd: &Fd) -> Option<TerminalSize>
where
    Fd: AsRawFd,
{
    let mut winsize = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `fd` is borrowed only for this ioctl call, and `winsize` points to
    // writable stack storage with the layout expected by TIOCGWINSZ.
    let result = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &mut winsize) };
    if result != 0 {
        return None;
    }
    let size = TerminalSize {
        cols: winsize.ws_col,
        rows: winsize.ws_row,
    };
    (size.cols > 0 && size.rows > 0).then_some(size)
}
