use std::os::fd::AsFd;

use rmux_proto::{TerminalGeometry, TerminalPixels, TerminalSize};
use rustix::termios::tcgetwinsize;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

pub(super) use platform::{ResizeWatcher, SignalMaskGuard};

use super::Result;

#[cfg(test)]
pub(super) fn terminal_size_from_fd<Fd>(fd: &Fd) -> Result<Option<TerminalSize>>
where
    Fd: AsFd,
{
    Ok(terminal_geometry_from_fd(fd)?.map(|geometry| geometry.size))
}

pub(super) fn terminal_geometry_from_fd<Fd>(fd: &Fd) -> Result<Option<TerminalGeometry>>
where
    Fd: AsFd,
{
    let winsize = tcgetwinsize(fd)?;
    let size = TerminalSize {
        cols: winsize.ws_col,
        rows: winsize.ws_row,
    };
    if size.cols == 0 || size.rows == 0 {
        return Ok(None);
    }
    let pixels = (winsize.ws_xpixel > 0 && winsize.ws_ypixel > 0)
        .then(|| TerminalPixels::new(winsize.ws_xpixel, winsize.ws_ypixel));
    Ok(Some(TerminalGeometry { size, pixels }))
}
