use std::io;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use rmux_core::input::InputParser;
use rmux_core::{GridRenderOptions, Screen, Style};
use rmux_proto::{RmuxError, TerminalSize};
#[cfg(unix)]
use rmux_pty::Signal;
use rmux_pty::{ChildCommand, PtyChild, PtyIo, RmuxShell, TerminalSize as PtyTerminalSize};
use tokio::sync::Notify;
use tokio::time::sleep;

mod reader;
use reader::spawn_popup_reader_task;
#[cfg(all(test, unix))]
use reader::{arm_popup_reader, read_async_fd, read_popup_batch};

use crate::terminal::{parse_environment_assignments, TerminalProfile};

use super::super::{attach_support::ActiveAttachIdentity, RequestHandler};
pub(super) use super::popup_io::PopupIoReceipt;
#[cfg(test)]
use super::popup_io::POPUP_IO_QUEUE_CAPACITY;
use super::popup_io::{PopupIoOperation, PopupIoQueue};

/// How often the job's exit waiter looks for its status; once the job's terminal has closed, how
/// often and how many times it looks before it falls back to its first interval.
const POPUP_EXIT_POLL: Duration = Duration::from_millis(50);
const POPUP_CLOSED_EXIT_POLL: Duration = Duration::from_millis(1);
const POPUP_CLOSED_EXIT_POLLS: usize = 200;
#[cfg(test)]
thread_local! {
    /// Replaces the first interval for the popups this test thread shows.
    pub(in crate::handler) static POPUP_EXIT_POLL_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

pub(in super::super) struct PopupSurface {
    parser: InputParser,
    screen: Screen,
    // One real renderer owns a pending reader flush. Further reads only dirty
    // this same authoritative surface until that renderer catches up.
    reader_flush_pending: bool,
    reader_flush_dirty: bool,
    #[cfg(test)]
    render_entries: std::cell::Cell<usize>,
    #[cfg(test)]
    publications: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(in crate::handler) flush_gate_for_test: Option<Arc<tokio::sync::Semaphore>>,
    #[cfg(test)]
    pub(in crate::handler) flush_done_for_test: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PopupSurfaceCursor {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PopupSurfaceFrame {
    pub(super) rows: Vec<Vec<u8>>,
    pub(super) cursor: PopupSurfaceCursor,
}

impl std::fmt::Debug for PopupSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopupSurface").finish_non_exhaustive()
    }
}

impl PopupSurface {
    pub(super) fn new(size: TerminalSize) -> Self {
        Self {
            parser: InputParser::new(),
            screen: Screen::new(size, 0),
            reader_flush_pending: false,
            reader_flush_dirty: false,
            #[cfg(test)]
            render_entries: std::cell::Cell::new(0),
            #[cfg(test)]
            publications: std::cell::Cell::new(0),
            #[cfg(test)]
            flush_gate_for_test: None,
            #[cfg(test)]
            flush_done_for_test: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub(super) fn mark_reader_dirty(&mut self) -> bool {
        self.reader_flush_dirty = true;
        !std::mem::replace(&mut self.reader_flush_pending, true)
    }

    pub(super) fn begin_reader_flush(&mut self) {
        self.reader_flush_dirty = false;
    }

    pub(super) fn finish_reader_flush(&mut self, still_current: bool) -> bool {
        self.reader_flush_dirty &= still_current;
        self.reader_flush_pending = self.reader_flush_dirty;
        self.reader_flush_pending
    }

    pub(super) fn append(&mut self, bytes: &[u8]) {
        self.parser.parse(bytes, &mut self.screen);
    }

    #[cfg(test)]
    pub(in crate::handler) fn append_for_test(&mut self, bytes: &[u8]) {
        self.append(bytes);
    }

    /// A blank surface of the same size that no job reader writes to.
    #[cfg(test)]
    pub(in crate::handler) fn detached_for_test(&self) -> Self {
        Self::new(self.screen.size())
    }

    // These count actual production entries, scoped to this surface, not a model.
    #[cfg(test)]
    pub(in crate::handler) fn production_counts_for_test(&self) -> (usize, usize) {
        (self.render_entries.get(), self.publications.get())
    }

    #[cfg(test)]
    pub(super) fn record_publication_for_test(&self) {
        self.publications.set(self.publications.get() + 1);
    }

    pub(super) fn resize(&mut self, size: TerminalSize) {
        self.screen.resize(size);
    }

    pub(super) fn mode(&self) -> u32 {
        self.screen.mode()
    }

    /// Visible rows and cursor state captured together from the popup screen.
    ///
    /// Popup content is terminal output, not a status format: capturing it
    /// without sequences discarded every colour and attribute the process
    /// emitted. Each row carries its own state because the renderer repositions
    /// the cursor per row, and `style` only paints the cells the process left
    /// unstyled, so `display-popup -s` keeps colouring the popup background.
    pub(super) fn frame(&self, style: &Style) -> PopupSurfaceFrame {
        #[cfg(test)]
        self.render_entries.set(self.render_entries.get() + 1);
        let options = GridRenderOptions {
            with_sequences: true,
            include_empty_cells: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        };
        let size = self.screen.size();
        let rows = (0..usize::from(size.rows))
            .filter_map(|row| {
                self.screen
                    .render_visible_line_independent_with_default_style(row, options, style)
            })
            .collect();
        let (cursor_x, cursor_y) = self.screen.cursor_position();
        PopupSurfaceFrame {
            rows,
            cursor: PopupSurfaceCursor {
                x: cursor_x.min(u32::from(size.cols.saturating_sub(1))) as u16,
                y: cursor_y.min(u32::from(size.rows.saturating_sub(1))) as u16,
                visible: self.screen.mode() & rmux_core::input::mode::MODE_CURSOR != 0,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PopupProcessControl {
    child: Arc<StdMutex<Option<PtyChild>>>,
    /// The job's terminal reported the end of its output: the job exited or closed it.
    output_closed: Arc<Notify>,
    #[cfg(unix)]
    terminating: Arc<AtomicBool>,
    #[cfg(unix)]
    closed: Arc<Notify>,
    #[cfg(windows)]
    close_child: Arc<StdMutex<Option<PtyChild>>>,
}

impl PopupProcessControl {
    #[cfg(unix)]
    async fn wait_closed(&self) {
        let notified = self.closed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.terminating.load(Ordering::Acquire) {
            notified.await;
        }
    }

    fn terminate(&self) {
        #[cfg(unix)]
        {
            if self.terminating.swap(true, Ordering::AcqRel) {
                return;
            }
            if let Some(child) = self.child.lock().expect("popup child").take() {
                // tmux job_free sends TERM to the leader, then closes the PTY.
                // Never escalate against a job that handles terminal closure.
                let _ = child.kill_session_leader(Signal::TERM);
                crate::unix_child_reaper::reap(child);
            }
            self.closed.notify_waiters();
        }
        #[cfg(windows)]
        {
            let child = self.child.lock().expect("popup child").take();
            let close_child = self.close_child.lock().expect("popup close child").take();
            spawn_popup_windows_teardown(child, close_child);
        }
    }

    #[cfg(test)]
    fn child_is_running(&self) -> bool {
        self.child
            .lock()
            .expect("popup child")
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }
}

#[derive(Debug)]
struct PopupProcessLifetime {
    control: PopupProcessControl,
}

impl Drop for PopupProcessLifetime {
    fn drop(&mut self) {
        self.control.terminate();
    }
}

#[derive(Debug, Clone)]
pub(in super::super) struct PopupJob {
    reader: Arc<PtyIo>,
    io_queue: PopupIoQueue,
    process: Arc<PopupProcessLifetime>,
}

impl PopupJob {
    pub(super) fn enqueue_write(&self, bytes: &[u8]) -> io::Result<PopupIoReceipt> {
        self.io_queue
            .enqueue(PopupIoOperation::Write(bytes.to_vec()))
    }

    pub(super) fn enqueue_resize(&self, size: TerminalSize) -> io::Result<PopupIoReceipt> {
        self.io_queue.enqueue(PopupIoOperation::Resize(size))
    }

    pub(in super::super) fn terminate(&self) {
        // Cancelling the queue drains every pending receipt before terminating
        // the child. The platform teardown then interrupts the one synchronous
        // PTY call that may already be running on a blocking worker.
        self.io_queue.cancel();
    }

    /// The job's terminal reported the end of its output: its exit waiter looks for its status now.
    #[cfg(unix)]
    pub(super) fn output_closed(&self) {
        self.process.control.output_closed.notify_one();
    }

    #[cfg(test)]
    pub(in crate::handler) fn child_is_running_for_test(&self) -> bool {
        self.process.control.child_is_running()
    }

    #[cfg(test)]
    pub(in crate::handler) fn with_test_writer<F>(&self, writer: F) -> Self
    where
        F: Fn(Vec<u8>) -> io::Result<()> + Send + Sync + 'static,
    {
        let process = Arc::clone(&self.process);
        let cancellation_process = process.control.clone();
        Self {
            reader: Arc::clone(&self.reader),
            io_queue: PopupIoQueue::spawn_with_cancel(
                move |operation| match operation {
                    PopupIoOperation::Write(bytes) => writer(bytes),
                    PopupIoOperation::Resize(_) => Ok(()),
                },
                move || cancellation_process.terminate(),
            ),
            process,
        }
    }

    #[cfg(test)]
    pub(in crate::handler) fn with_test_resize<F>(&self, resize: F) -> Self
    where
        F: Fn(TerminalSize) -> io::Result<()> + Send + Sync + 'static,
    {
        let process = Arc::clone(&self.process);
        let cancellation_process = process.control.clone();
        Self {
            reader: Arc::clone(&self.reader),
            io_queue: PopupIoQueue::spawn_with_cancel(
                move |operation| match operation {
                    PopupIoOperation::Write(_) => Ok(()),
                    PopupIoOperation::Resize(size) => resize(size),
                },
                move || cancellation_process.terminate(),
            ),
            process,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum PopupDragMode {
    Off,
    Move { dx: u16, dy: u16 },
    Resize,
}

/// The popup job's process: the target's default-shell running the command with -c under its file
/// name as `argv[0]` (JOB-ARGV0), else the shell alone, started as tmux starts a popup's shell.
pub(super) fn popup_job_command(
    profile: &TerminalProfile,
    shell_command: Option<&str>,
) -> ChildCommand {
    shell_command
        .map(|command| profile.popup_job_child_command(command))
        .unwrap_or_else(|| profile.popup_interactive_child_command())
}

pub(super) fn spawn_popup_job(
    shell: &dyn RmuxShell,
    size: TerminalSize,
    profile: &TerminalProfile,
    shell_command: Option<&str>,
    environment: &[String],
) -> Result<(PopupJob, Vec<u8>), RmuxError> {
    let env = parse_environment_assignments(environment)?;
    let mut command = popup_job_command(profile, shell_command)
        .size(PtyTerminalSize::new(size.cols.max(1), size.rows.max(1)))
        .clear_env()
        .current_dir(profile.cwd());
    for (name, value) in profile.environment() {
        command = command.env(name, value);
    }
    for (name, value) in env {
        command = command.env(name, value);
    }
    let spawned = shell
        .spawn(command)
        .map_err(|error| RmuxError::Server(format!("failed to spawn popup process: {error}")))?;
    let (mut master, child) = spawned.into_parts();
    // The reader endpoint takes the startup slave and releases it after its
    // first read (reader.rs). Darwin discards what a job queued on its terminal
    // when the job's last slave close comes before any master read, so a job
    // that ends before its popup is installed would otherwise show nothing.
    let reader = master
        .try_clone_for_startup_reader()
        .map_err(|error| RmuxError::Server(format!("failed to clone popup pty fd: {error}")))?
        .into_io();
    let writer_fd = master.into_io();
    #[cfg(unix)]
    writer_fd
        .set_nonblocking()
        .map_err(|error| RmuxError::Server(format!("failed to prepare popup pty: {error}")))?;
    #[cfg(windows)]
    let close_child = child.try_clone_for_wait().map_err(|error| {
        RmuxError::Server(format!(
            "failed to clone popup child for ConPTY teardown: {error}"
        ))
    })?;
    // Execute every potentially blocking write/resize in one FIFO worker so
    // synchronous ConPTY and Unix readiness waits never occupy a Tokio runtime
    // thread. The reader endpoint is cloned before the worker can accept input,
    // so setup never waits behind a blocked write.
    let process_control = PopupProcessControl {
        child: Arc::new(StdMutex::new(Some(child))),
        output_closed: Arc::new(Notify::new()),
        #[cfg(unix)]
        terminating: Arc::new(AtomicBool::new(false)),
        #[cfg(unix)]
        closed: Arc::new(Notify::new()),
        #[cfg(windows)]
        close_child: Arc::new(StdMutex::new(Some(close_child))),
    };
    let writer = Arc::new(writer_fd);
    let cancellation_process = process_control.clone();
    let io_queue = PopupIoQueue::spawn_with_cancel(
        move |operation| match operation {
            PopupIoOperation::Write(bytes) => writer.write_all(&bytes),
            PopupIoOperation::Resize(size) => writer
                .resize(PtyTerminalSize::new(size.cols.max(1), size.rows.max(1)))
                .map_err(io::Error::other),
        },
        move || cancellation_process.terminate(),
    );
    let process = Arc::new(PopupProcessLifetime {
        control: process_control,
    });
    Ok((
        PopupJob {
            reader: Arc::new(reader),
            io_queue,
            process,
        },
        Vec::new(),
    ))
}

impl RequestHandler {
    pub(super) fn spawn_popup_reader(
        &self,
        identity: ActiveAttachIdentity,
        popup_id: u64,
        surface: Arc<StdMutex<PopupSurface>>,
        job: PopupJob,
    ) -> Result<(), RmuxError> {
        let reader_fd = job
            .reader
            .try_clone()
            .map_err(|error| RmuxError::Server(format!("failed to clone popup pty fd: {error}")))?;
        spawn_popup_reader_task(
            self.clone(),
            identity,
            popup_id,
            surface,
            reader_fd,
            Arc::clone(&job.reader),
            #[cfg(unix)]
            job.process.control.clone(),
        )
    }

    pub(super) fn spawn_popup_waiter(
        &self,
        identity: ActiveAttachIdentity,
        popup_id: u64,
        job: PopupJob,
    ) {
        // The overlay owns the PopupJob lifetime. The waiter keeps only the
        // process handles it needs, so dropping an overlay without an explicit
        // teardown still terminates the process and stops the I/O queue.
        let process = job.process.control.clone();
        drop(job);
        let handler = self.clone();
        #[cfg(test)]
        let exit_poll = POPUP_EXIT_POLL_FOR_TEST.get().unwrap_or(POPUP_EXIT_POLL);
        #[cfg(not(test))]
        let exit_poll = POPUP_EXIT_POLL;
        tokio::spawn(async move {
            let mut closed_polls = 0;
            loop {
                let status = {
                    let mut child_guard = process.child.lock().expect("popup child");
                    let Some(child) = child_guard.as_mut() else {
                        return;
                    };
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            #[cfg(windows)]
                            {
                                let child = child_guard.take();
                                let close_child = process
                                    .close_child
                                    .lock()
                                    .expect("popup close child")
                                    .take();
                                spawn_popup_windows_teardown(child, close_child);
                            }
                            #[cfg(unix)]
                            let _ = child_guard.take();
                            status_to_code(status)
                        }
                        Ok(None) => None,
                        Err(_) => None,
                    }
                };
                if let Some(status) = status {
                    let _ = handler.popup_job_finished(identity, popup_id, status).await;
                    return;
                }
                if closed_polls > 0 {
                    closed_polls -= 1;
                    sleep(POPUP_CLOSED_EXIT_POLL).await;
                    continue;
                }
                tokio::select! {
                    () = sleep(exit_poll) => {}
                    () = process.output_closed.notified() => closed_polls = POPUP_CLOSED_EXIT_POLLS,
                }
            }
        });
    }
}

#[cfg(windows)]
fn spawn_popup_windows_teardown(child: Option<PtyChild>, close_child: Option<PtyChild>) {
    if child.is_none() && close_child.is_none() {
        return;
    }
    if let Err(error) = std::thread::Builder::new()
        .name("rmux-popup-conpty-teardown".to_owned())
        .spawn(move || {
            // Dropping the owning child closes its kill-on-close Job Object
            // before ClosePseudoConsole can wait on surviving descendants.
            drop(child);
            if let Some(close_child) = close_child {
                close_child.close_pseudoconsole();
            }
        })
    {
        tracing::warn!(
            target: "rmux::conpty",
            "failed to spawn popup ConPTY teardown thread: {error}"
        );
    }
}

fn status_to_code(status: std::process::ExitStatus) -> Option<i32> {
    status.code().or_else(|| exit_signal(status))
}

#[cfg(unix)]
fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    status.signal()
}

#[cfg(windows)]
fn exit_signal(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

#[cfg(test)]
#[path = "../../handler_overlay/popup_job_tests.rs"]
mod tests;
