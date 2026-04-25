use std::os::fd::AsFd;

use rmux_proto::TerminalSize;
use rustix::termios::tcgetwinsize;

#[cfg(target_os = "linux")]
#[path = "resize/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "resize/macos.rs"]
mod platform;

pub(super) use platform::{ResizeWatcher, SignalMaskGuard};

use super::Result;

pub(super) fn terminal_size_from_fd<Fd>(fd: &Fd) -> Result<TerminalSize>
where
    Fd: AsFd,
{
    let winsize = tcgetwinsize(fd)?;
    Ok(TerminalSize {
        cols: winsize.ws_col,
        rows: winsize.ws_row,
    })
}
