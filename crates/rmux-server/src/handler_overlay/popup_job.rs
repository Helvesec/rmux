use std::io;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use rmux_core::input::InputParser;
use rmux_core::{GridRenderOptions, Screen, ScreenCaptureRange};
use rmux_proto::{RmuxError, TerminalSize};
use rmux_pty::{PtyChild, PtyIo, Signal, TerminalSize as PtyTerminalSize};
#[cfg(unix)]
use tokio::io::unix::AsyncFd;
use tokio::time::sleep;

use crate::terminal::{parse_environment_assignments, TerminalProfile};

use super::super::{attach_support::ActiveAttachIdentity, RequestHandler};

pub(in super::super) struct PopupSurface {
    parser: InputParser,
    screen: Screen,
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
        }
    }

    pub(super) fn append(&mut self, bytes: &[u8]) {
        self.parser.parse(bytes, &mut self.screen);
    }

    pub(super) fn resize(&mut self, size: TerminalSize) {
        self.screen.resize(size);
    }

    pub(super) fn mode(&self) -> u32 {
        self.screen.mode()
    }

    pub(super) fn lines(&self) -> Vec<String> {
        let bytes = self
            .screen
            .capture_transcript(ScreenCaptureRange::default(), GridRenderOptions::default());
        let rendered = String::from_utf8_lossy(&bytes);
        let mut lines = rendered
            .split('\n')
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        while lines.last().is_some_and(String::is_empty) {
            let _ = lines.pop();
        }
        lines
    }
}

#[derive(Debug, Clone)]
pub(in super::super) struct PopupJob {
    writer: Arc<StdMutex<PtyIo>>,
    child: Arc<StdMutex<Option<PtyChild>>>,
}

impl PopupJob {
    pub(super) fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let writer = self.writer.lock().expect("popup writer");
        writer.write_all(bytes)
    }

    pub(super) fn resize(&self, size: TerminalSize) -> io::Result<()> {
        let writer = self.writer.lock().expect("popup writer");
        writer
            .resize(PtyTerminalSize::new(size.cols.max(1), size.rows.max(1)))
            .map_err(io::Error::other)
    }

    pub(in super::super) fn terminate(&self) {
        if let Some(child) = self.child.lock().expect("popup child").as_ref() {
            let _ = child.kill(Signal::HUP);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum PopupDragMode {
    Off,
    Move { dx: u16, dy: u16 },
    Resize,
}

pub(super) fn spawn_popup_job(
    size: TerminalSize,
    profile: &TerminalProfile,
    shell_command: Option<&str>,
    environment: &[String],
) -> Result<(PopupJob, Vec<u8>), RmuxError> {
    let env = parse_environment_assignments(environment)?;
    let mut command = shell_command
        .map(|command| profile.shell_child_command(command))
        .unwrap_or_else(|| profile.interactive_child_command())
        .size(PtyTerminalSize::new(size.cols.max(1), size.rows.max(1)))
        .clear_env()
        .current_dir(profile.cwd());
    for (name, value) in profile.environment() {
        command = command.env(name, value);
    }
    for (name, value) in env {
        command = command.env(name, value);
    }
    let spawned = command
        .spawn()
        .map_err(|error| RmuxError::Server(format!("failed to spawn popup process: {error}")))?;
    let (master, child) = spawned.into_parts();
    let writer_fd = master.into_io();
    #[cfg(unix)]
    writer_fd
        .set_nonblocking()
        .map_err(|error| RmuxError::Server(format!("failed to prepare popup pty: {error}")))?;
    Ok((
        PopupJob {
            writer: Arc::new(StdMutex::new(writer_fd)),
            child: Arc::new(StdMutex::new(Some(child))),
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
        let reader_fd = {
            let writer = job.writer.lock().expect("popup writer");
            writer.try_clone().map_err(|error| {
                RmuxError::Server(format!("failed to clone popup pty fd: {error}"))
            })?
        };
        spawn_popup_reader_task(self.clone(), identity, popup_id, surface, reader_fd)
    }

    pub(super) fn spawn_popup_waiter(
        &self,
        identity: ActiveAttachIdentity,
        popup_id: u64,
        job: PopupJob,
    ) {
        let handler = self.clone();
        tokio::spawn(async move {
            loop {
                let status = {
                    let mut child_guard = job.child.lock().expect("popup child");
                    let Some(child) = child_guard.as_mut() else {
                        return;
                    };
                    match child.try_wait() {
                        Ok(Some(status)) => {
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
                sleep(Duration::from_millis(50)).await;
            }
        });
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

#[cfg(unix)]
fn spawn_popup_reader_task(
    handler: RequestHandler,
    identity: ActiveAttachIdentity,
    popup_id: u64,
    surface: Arc<StdMutex<PopupSurface>>,
    reader_fd: PtyIo,
) -> Result<(), RmuxError> {
    reader_fd.set_nonblocking().map_err(|error| {
        RmuxError::Server(format!("failed to make popup pty nonblocking: {error}"))
    })?;
    let reader = AsyncFd::new(reader_fd)
        .map_err(|error| RmuxError::Server(format!("failed to watch popup pty: {error}")))?;
    tokio::spawn(async move {
        let mut buffer = [0_u8; 8192];
        loop {
            let bytes_read = match read_async_fd(&reader, &mut buffer).await {
                Ok(bytes_read) => bytes_read,
                Err(_) => break,
            };
            if bytes_read == 0 {
                break;
            }
            surface
                .lock()
                .expect("popup surface")
                .append(&buffer[..bytes_read]);
            let _ = handler.popup_reader_tick(identity, popup_id).await;
        }
    });
    Ok(())
}

#[cfg(windows)]
fn spawn_popup_reader_task(
    handler: RequestHandler,
    identity: ActiveAttachIdentity,
    popup_id: u64,
    surface: Arc<StdMutex<PopupSurface>>,
    reader: PtyIo,
) -> Result<(), RmuxError> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let bytes_read = match reader.read(&mut buffer) {
                Ok(bytes_read) => bytes_read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if bytes_read == 0 {
                break;
            }
            surface
                .lock()
                .expect("popup surface")
                .append(&buffer[..bytes_read]);
            let handler = handler.clone();
            runtime.block_on(async move {
                let _ = handler.popup_reader_tick(identity, popup_id).await;
            });
        }
    });
    Ok(())
}

#[cfg(unix)]
async fn read_async_fd(fd: &AsyncFd<PtyIo>, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut ready = fd.readable().await?;
        match ready.try_io(|inner| inner.get_ref().read(&mut *buffer)) {
            Ok(result) => return result,
            Err(_would_block) => continue,
        }
    }
}
