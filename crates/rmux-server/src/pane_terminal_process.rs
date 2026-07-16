#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use rmux_core::PaneGeometry;
use rmux_proto::{ProcessCommand, RmuxError, TerminalPixels, TerminalSize};
use rmux_pty::{
    PtyChild, PtyMaster, Signal, TerminalGeometry as PtyTerminalGeometry,
    TerminalSize as PtyTerminalSize,
};

use crate::terminal::{spawn_pane_process, TerminalProfile};

const GRACEFUL_TERMINATION_TIMEOUT: Duration = Duration::from_millis(100);
const HARD_TERMINATION_TIMEOUT: Duration = Duration::from_millis(500);
const TERMINATION_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub(crate) struct PaneTerminal {
    master: PtyMaster,
    child: PtyChild,
    exit_status: Option<ExitStatus>,
    termination_attempted: bool,
    runtime_window_name: Option<String>,
    #[cfg_attr(not(test), allow(dead_code))]
    profile: TerminalProfile,
}

impl PaneTerminal {
    pub(crate) fn new(
        master: PtyMaster,
        child: PtyChild,
        runtime_window_name: Option<String>,
        profile: TerminalProfile,
    ) -> Self {
        Self {
            master,
            child,
            exit_status: None,
            termination_attempted: false,
            runtime_window_name,
            profile,
        }
    }

    pub(crate) fn resize(&self, geometry: PtyTerminalGeometry) -> rmux_pty::Result<()> {
        self.master.resize_geometry(geometry)
    }

    #[cfg(unix)]
    pub(crate) fn master_fd(&self) -> BorrowedFd<'_> {
        self.master.io().as_fd()
    }

    pub(crate) fn clone_master(&self) -> rmux_pty::Result<PtyMaster> {
        self.master.try_clone()
    }

    pub(crate) fn clone_master_for_output_reader(&mut self) -> rmux_pty::Result<PtyMaster> {
        self.master.try_clone_for_startup_reader()
    }

    #[cfg(windows)]
    pub(crate) fn clone_child_for_exit_teardown(&self) -> rmux_pty::Result<PtyChild> {
        self.child.try_clone_for_exit_teardown()
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.pid().as_u32()
    }

    #[cfg(unix)]
    pub(crate) fn tty_path(&self) -> Option<PathBuf> {
        rmux_os::process::fd_path(self.pid(), 0)
    }

    pub(crate) fn is_alive(&mut self) -> rmux_pty::Result<bool> {
        if self.exit_status.is_some() {
            return Ok(false);
        }

        match self.child.try_wait()? {
            Some(status) => {
                self.exit_status = Some(status);
                Ok(false)
            }
            None => Ok(true),
        }
    }

    pub(crate) fn exit_status(&mut self) -> rmux_pty::Result<Option<ExitStatus>> {
        let _ = self.is_alive()?;
        Ok(self.exit_status)
    }

    #[cfg(unix)]
    pub(crate) fn continue_if_stopped(&self) -> rmux_pty::Result<bool> {
        self.child.continue_if_stopped()
    }

    pub(crate) fn profile(&self) -> &TerminalProfile {
        &self.profile
    }

    pub(crate) fn runtime_window_name(&self) -> Option<&str> {
        self.runtime_window_name.as_deref()
    }

    pub(crate) fn terminate_with_bounded_grace(&mut self) {
        if self.exit_status.is_some() || self.termination_attempted {
            return;
        }
        self.termination_attempted = true;

        self.signal_process_tree(Signal::HUP);
        if self.wait_for_exit(GRACEFUL_TERMINATION_TIMEOUT) {
            return;
        }

        self.signal_process_tree(Signal::KILL);
        let _ = self.wait_for_exit(HARD_TERMINATION_TIMEOUT);
    }

    pub(crate) fn terminate_in_background(self) {
        let _ = std::thread::Builder::new()
            .name("rmux-pane-terminate".to_owned())
            .spawn(move || {
                let mut terminal = self;
                terminal.terminate_with_bounded_grace();
            });
    }

    fn signal_process_tree(&self, signal: Signal) {
        let _ = self.child.kill(signal);
        let _ = self.child.kill_session_leader(signal);
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.exit_status = Some(status);
                    return true;
                }
                Ok(None) => {}
                Err(_) => return true,
            }

            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep(TERMINATION_POLL_INTERVAL.min(deadline - now));
        }
    }
}

impl Drop for PaneTerminal {
    fn drop(&mut self) {
        self.terminate_with_bounded_grace();
    }
}

pub(crate) fn open_pane_terminal(
    geometry: PaneGeometry,
    profile: TerminalProfile,
    runtime_window_name: Option<String>,
    command: Option<&ProcessCommand>,
) -> Result<PaneTerminal, RmuxError> {
    let (master, child) = spawn_pane_process(pty_size_from_geometry(geometry), &profile, command)?;
    Ok(PaneTerminal::new(
        master,
        child,
        runtime_window_name,
        profile,
    ))
}

pub(crate) fn pty_size_from_geometry(geometry: PaneGeometry) -> PtyTerminalSize {
    PtyTerminalSize::new(geometry.cols().max(1), geometry.rows().max(1))
}

pub(crate) fn pty_geometry_from_layout(
    geometry: PaneGeometry,
    session_size: TerminalSize,
    terminal_pixels: Option<TerminalPixels>,
) -> PtyTerminalGeometry {
    let size = pty_size_from_geometry(geometry);
    let Some(terminal_pixels) = terminal_pixels else {
        return PtyTerminalGeometry::from_size(size);
    };
    let Some(pixels) = scale_pane_pixels(size, session_size, terminal_pixels) else {
        return PtyTerminalGeometry::from_size(size);
    };
    PtyTerminalGeometry::from_size(size).with_pixels(pixels)
}

fn scale_pane_pixels(
    pane_size: TerminalSize,
    session_size: TerminalSize,
    terminal_pixels: TerminalPixels,
) -> Option<TerminalPixels> {
    let width = scale_dimension_pixels(pane_size.cols, session_size.cols, terminal_pixels.width)?;
    let height = scale_dimension_pixels(pane_size.rows, session_size.rows, terminal_pixels.height)?;
    Some(TerminalPixels::new(width, height))
}

fn scale_dimension_pixels(cells: u16, total_cells: u16, total_pixels: u16) -> Option<u16> {
    if cells == 0 || total_cells == 0 || total_pixels == 0 {
        return None;
    }
    let scaled = u32::from(total_pixels).saturating_mul(u32::from(cells)) / u32::from(total_cells);
    Some(u16::try_from(scaled.max(1)).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use rmux_core::PaneGeometry;
    use rmux_proto::{TerminalPixels, TerminalSize};

    use super::{pty_geometry_from_layout, pty_size_from_geometry};

    #[test]
    fn pty_geometry_scales_terminal_pixels_to_pane_cells() {
        let geometry = pty_geometry_from_layout(
            PaneGeometry::new(0, 0, 60, 20),
            TerminalSize::new(120, 40),
            Some(TerminalPixels::new(1440, 960)),
        );

        assert_eq!(geometry.size, TerminalSize::new(60, 20));
        assert_eq!(geometry.pixels, Some(TerminalPixels::new(720, 480)));
    }

    #[test]
    fn pty_size_from_geometry_never_opens_or_resizes_zero_sized_backend() {
        assert_eq!(
            pty_size_from_geometry(PaneGeometry::new(0, 0, 0, 0)),
            TerminalSize::new(1, 1)
        );
        assert_eq!(
            pty_size_from_geometry(PaneGeometry::new(0, 23, 80, 0)),
            TerminalSize::new(80, 1)
        );
    }
}
