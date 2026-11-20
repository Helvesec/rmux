#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmux_core::PaneGeometry;
use rmux_proto::{ProcessCommand, RmuxError, TerminalPixels, TerminalSize};
use rmux_pty::{
    ProcessShell, PtyChild, PtyMaster, RmuxShell, Signal, TerminalGeometry as PtyTerminalGeometry,
    TerminalSize as PtyTerminalSize,
};

use crate::terminal::{spawn_pane_process, TerminalProfile};

const GRACEFUL_TERMINATION_TIMEOUT: Duration = Duration::from_millis(100);
const HARD_TERMINATION_TIMEOUT: Duration = Duration::from_millis(500);
const TERMINATION_POLL_INTERVAL: Duration = Duration::from_millis(1);
/// A signalled Unix shell normally exits within microseconds, so exit
/// observation starts with a short wait that doubles up to
/// `TERMINATION_POLL_INTERVAL`. Windows keeps its steady interval.
#[cfg(unix)]
const FIRST_TERMINATION_POLL_INTERVAL: Duration = Duration::from_micros(50);
#[cfg(not(unix))]
const FIRST_TERMINATION_POLL_INTERVAL: Duration = TERMINATION_POLL_INTERVAL;

/// Waits between exit observations: `FIRST_TERMINATION_POLL_INTERVAL`, doubled
/// after each wait up to `TERMINATION_POLL_INTERVAL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExitPollBackoff {
    interval: Duration,
}

impl ExitPollBackoff {
    const fn new() -> Self {
        Self {
            interval: FIRST_TERMINATION_POLL_INTERVAL,
        }
    }

    /// Returns the next wait, never longer than `remaining`, and advances.
    fn next_wait(&mut self, remaining: Duration) -> Duration {
        let wait = self.interval.min(remaining);
        self.interval = self
            .interval
            .saturating_mul(2)
            .min(TERMINATION_POLL_INTERVAL);
        wait
    }
}

/// Calls `observe` until it reports every exit settled or `deadline` passes,
/// waiting with `sleep` between observations. An observation always follows
/// the last wait, and no wait extends past `deadline`. Returns whether every
/// exit settled.
fn poll_exit_until(
    deadline: Instant,
    mut sleep: impl FnMut(Duration),
    mut observe: impl FnMut() -> bool,
) -> bool {
    let mut backoff = ExitPollBackoff::new();
    loop {
        if observe() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        sleep(backoff.next_wait(deadline - now));
    }
}

#[cfg(unix)]
mod shutdown;

#[derive(Debug)]
pub(crate) struct PaneTerminal {
    // A split -I pane owns its profile and output, but no OS process or PTY.
    process: Option<PaneProcess>,
    render_snapshot: Option<PaneRenderProcess>,
    runtime_window_name: Option<String>,
    #[cfg_attr(not(test), allow(dead_code))]
    profile: TerminalProfile,
}

#[derive(Debug)]
struct PaneRenderProcess {
    master: Arc<PtyMaster>,
    pid: u32,
}

#[derive(Debug)]
struct PaneProcess {
    master: Arc<PtyMaster>,
    child: PtyChild,
    exit_status: Option<ExitStatus>,
    termination_attempted: bool,
}

impl PaneTerminal {
    pub(crate) fn render_snapshot(&self) -> Self {
        Self {
            process: None,
            render_snapshot: self.process.as_ref().map(|process| PaneRenderProcess {
                master: Arc::clone(&process.master),
                pid: self.pid(),
            }),
            runtime_window_name: self.runtime_window_name.clone(),
            profile: self.profile.clone(),
        }
    }

    pub(crate) fn new(
        master: PtyMaster,
        child: PtyChild,
        runtime_window_name: Option<String>,
        profile: TerminalProfile,
    ) -> Self {
        Self {
            render_snapshot: None,
            process: Some(PaneProcess {
                master: Arc::new(master),
                child,
                exit_status: None,
                termination_attempted: false,
            }),
            runtime_window_name,
            profile,
        }
    }

    pub(crate) fn empty(profile: TerminalProfile) -> Self {
        Self {
            render_snapshot: None,
            process: None,
            runtime_window_name: None,
            profile,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.process.is_none() && self.render_snapshot.is_none()
    }

    fn process(&self) -> rmux_pty::Result<&PaneProcess> {
        self.process.as_ref().ok_or_else(|| {
            rmux_pty::PtyError::Spawn(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "empty pane has no terminal",
            ))
        })
    }

    pub(crate) fn resize(&self, geometry: PtyTerminalGeometry) -> rmux_pty::Result<()> {
        match &self.process {
            Some(process) => process.master.resize_geometry(geometry),
            None => Ok(()),
        }
    }

    #[cfg(unix)]
    pub(crate) fn master_fd(&self) -> Option<BorrowedFd<'_>> {
        self.process
            .as_ref()
            .map(|process| process.master.io().as_fd())
            .or_else(|| {
                self.render_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.master.io().as_fd())
            })
    }

    pub(crate) fn clone_master(&self) -> rmux_pty::Result<PtyMaster> {
        if let Some(snapshot) = &self.render_snapshot {
            return snapshot.master.try_clone();
        }
        self.process()?.master.try_clone()
    }

    pub(crate) fn clone_master_for_output_reader(&mut self) -> rmux_pty::Result<PtyMaster> {
        self.process()?;
        let master = &mut self.process.as_mut().expect("process checked above").master;
        // Startup transfers its slave keepalive to the reader before the
        // terminal is installed in HandlerState and can be captured by Web.
        Arc::get_mut(master)
            .ok_or_else(|| {
                rmux_pty::PtyError::Spawn(std::io::Error::other(
                    "pane output reader must start before a render capture",
                ))
            })?
            .try_clone_for_startup_reader()
    }

    #[cfg(windows)]
    pub(crate) fn clone_child_for_exit_teardown(&self) -> rmux_pty::Result<PtyChild> {
        self.process()?.child.try_clone_for_exit_teardown()
    }

    pub(crate) fn pid(&self) -> u32 {
        self.process
            .as_ref()
            .map(|process| process.child.pid().as_u32())
            .or_else(|| self.render_snapshot.as_ref().map(|snapshot| snapshot.pid))
            .unwrap_or(0)
    }

    /// The terminal this pane owns, by the name of the slave of the PTY pair it
    /// was allocated with.
    ///
    /// It used to be read as the current destination of the child's descriptor
    /// 0, which is not the pane's terminal but wherever that one descriptor
    /// happens to point: a child that replaces its standard input made the pane
    /// report that replacement instead. The master keeps the name from its own
    /// allocation, so the answer belongs to the pane and no descriptor of the
    /// child is consulted.
    #[cfg(unix)]
    pub(crate) fn tty_path(&self) -> Option<PathBuf> {
        self.process
            .as_ref()
            .and_then(|process| process.master.slave_path().map(Path::to_path_buf))
            .or_else(|| {
                self.render_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.master.slave_path().map(Path::to_path_buf))
            })
    }

    pub(crate) fn is_alive(&mut self) -> rmux_pty::Result<bool> {
        self.process
            .as_mut()
            .map_or(Ok(false), PaneProcess::is_alive)
    }

    pub(crate) fn exit_status(&mut self) -> rmux_pty::Result<Option<ExitStatus>> {
        self.process.as_mut().map_or(Ok(None), |process| {
            let _ = process.is_alive()?;
            Ok(process.exit_status)
        })
    }

    #[cfg(unix)]
    pub(crate) fn continue_if_stopped(&self) -> rmux_pty::Result<bool> {
        self.process
            .as_ref()
            .map_or(Ok(false), |process| process.child.continue_if_stopped())
    }

    pub(crate) fn profile(&self) -> &TerminalProfile {
        &self.profile
    }

    pub(crate) fn runtime_window_name(&self) -> Option<&str> {
        self.runtime_window_name.as_deref()
    }

    pub(crate) fn terminate_in_background(mut self) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("rmux-pane-terminate".to_owned())
            .spawn(move || process.terminate_with_bounded_grace());
    }
}

impl PaneProcess {
    fn is_alive(&mut self) -> rmux_pty::Result<bool> {
        if self.exit_status.is_some() {
            return Ok(false);
        }

        match self.try_wait_for_terminal_exit()? {
            Some(status) => {
                self.exit_status = Some(status);
                Ok(false)
            }
            None => Ok(true),
        }
    }

    fn terminate_with_bounded_grace(&mut self) {
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

    fn signal_process_tree(&self, signal: Signal) {
        let _ = self.child.kill(signal);
        let _ = self.child.kill_session_leader(signal);
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        poll_exit_until(deadline, std::thread::sleep, || {
            match self.try_wait_for_terminal_exit() {
                Ok(Some(status)) => {
                    self.exit_status = Some(status);
                    true
                }
                Ok(None) => false,
                Err(_) => true,
            }
        })
    }

    fn try_wait_for_terminal_exit(&mut self) -> rmux_pty::Result<Option<ExitStatus>> {
        #[cfg(windows)]
        {
            self.child.try_wait_for_process_tree_exit()
        }

        #[cfg(not(windows))]
        {
            self.child.try_wait()
        }
    }
}

impl Drop for PaneProcess {
    fn drop(&mut self) {
        self.terminate_with_bounded_grace();
    }
}

/// The [`RmuxShell`] backend every pane and popup of this server is spawned
/// with. Defaults to the in-tree [`ProcessShell`]; `ServerDaemon::bind_with_shell`
/// replaces it.
#[derive(Clone, Debug)]
pub(crate) struct PaneShell(pub(crate) Arc<dyn RmuxShell>);

impl Default for PaneShell {
    fn default() -> Self {
        Self(Arc::new(ProcessShell))
    }
}

pub(crate) fn open_pane_terminal(
    shell: &dyn RmuxShell,
    geometry: PaneGeometry,
    profile: TerminalProfile,
    runtime_window_name: Option<String>,
    command: Option<&ProcessCommand>,
) -> Result<PaneTerminal, RmuxError> {
    let (master, child) =
        spawn_pane_process(shell, pty_size_from_geometry(geometry), &profile, command)?;
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

#[cfg(all(test, unix))]
#[path = "pane_terminal_process/ptty1_tests.rs"]
mod ptty1_tests;

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use rmux_core::PaneGeometry;
    use rmux_proto::{TerminalPixels, TerminalSize};

    use super::{
        poll_exit_until, pty_geometry_from_layout, pty_size_from_geometry, ExitPollBackoff,
        TERMINATION_POLL_INTERVAL,
    };

    /// Waits between the first seven exit observations when the deadline is
    /// far away. Unix starts short because a signalled shell exits quickly,
    /// and kill-server waits for this reap; Windows keeps its steady interval.
    #[cfg(unix)]
    const EXPECTED_WAITS_US: [u64; 7] = [50, 100, 200, 400, 800, 1_000, 1_000];
    #[cfg(not(unix))]
    const EXPECTED_WAITS_US: [u64; 7] = [1_000; 7];

    fn expected_waits() -> Vec<Duration> {
        EXPECTED_WAITS_US
            .iter()
            .map(|&micros| Duration::from_micros(micros))
            .collect()
    }

    #[test]
    fn exit_poll_backoff_waits_the_platform_sequence_capped_at_the_poll_interval() {
        let mut backoff = ExitPollBackoff::new();
        let waits: Vec<Duration> = (0..EXPECTED_WAITS_US.len())
            .map(|_| backoff.next_wait(Duration::from_secs(1)))
            .collect();

        assert_eq!(waits, expected_waits());
        assert_eq!(backoff.interval, TERMINATION_POLL_INTERVAL);
    }

    #[test]
    fn exit_poll_backoff_never_waits_past_the_remaining_time() {
        let mut backoff = ExitPollBackoff::new();
        assert_eq!(
            backoff.next_wait(Duration::from_micros(30)),
            Duration::from_micros(30)
        );
        assert_eq!(backoff.next_wait(Duration::ZERO), Duration::ZERO);
        for _ in 0..8 {
            backoff.next_wait(Duration::from_secs(1));
        }
        assert_eq!(
            backoff.next_wait(Duration::from_micros(999)),
            Duration::from_micros(999)
        );
    }

    /// The deterministic regression guard: the shared exit poll loop observes
    /// first, then alternates one backoff wait and one observation, without
    /// any scheduler or clock bound in the assertion.
    #[test]
    fn poll_exit_until_alternates_observations_with_backoff_waits() {
        #[derive(Debug, PartialEq, Eq)]
        enum Step {
            Observe,
            Wait(Duration),
        }
        let steps = RefCell::new(Vec::new());
        let unsettled_observations = EXPECTED_WAITS_US.len();
        let mut observations = 0;

        let settled = poll_exit_until(
            Instant::now() + Duration::from_secs(3_600),
            |wait| steps.borrow_mut().push(Step::Wait(wait)),
            || {
                steps.borrow_mut().push(Step::Observe);
                observations += 1;
                observations > unsettled_observations
            },
        );

        assert!(settled);
        let mut expected = vec![Step::Observe];
        for wait in expected_waits() {
            expected.extend([Step::Wait(wait), Step::Observe]);
        }
        assert_eq!(steps.into_inner(), expected);
    }

    #[test]
    fn poll_exit_until_observes_once_without_waiting_after_its_deadline() {
        for settles in [false, true] {
            let mut observations = 0;
            let mut waits = 0;
            let settled = poll_exit_until(
                Instant::now(),
                |_| waits += 1,
                || {
                    observations += 1;
                    settles
                },
            );

            assert_eq!((settled, observations, waits), (settles, 1, 0));
        }
    }

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
