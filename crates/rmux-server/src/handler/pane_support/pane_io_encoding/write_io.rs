//! Delivery of prepared pane input writes to PTYs and Windows consoles.

#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use rmux_proto::SessionName;
use rmux_proto::{ErrorResponse, Response, RmuxError, SendKeysResponse};
#[cfg(windows)]
use rmux_pty::ProcessId;
use rmux_pty::PtyMaster;

use super::write_failure::PaneInputWriteFailure;
#[cfg(windows)]
use super::{PaneConsoleInputSink, PaneConsoleInputWrite, WindowsConsoleInputAction};
use super::{PaneInputSink, PaneInputWrite};

#[cfg(windows)]
const PANE_INPUT_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(windows)]
const PANE_INPUT_WRITE_RECOVERY_GRACE: Duration = Duration::from_millis(500);

pub(in crate::handler::pane_support) async fn write_bytes_to_target(
    write: PaneInputWrite,
    bytes: Vec<u8>,
    key_count: usize,
) -> Response {
    match write_bytes_to_target_io(write, bytes).await {
        Ok(()) => Response::SendKeys(SendKeysResponse { key_count }),
        Err(error) => Response::Error(ErrorResponse { error }),
    }
}

pub(in crate::handler::pane_support) async fn write_bytes_to_targets(
    writes: Vec<PaneInputWrite>,
    bytes: Vec<u8>,
    key_count: usize,
) -> Response {
    for write in writes {
        if let Err(error) = write_bytes_to_target_io(write, bytes.clone()).await {
            return Response::Error(ErrorResponse { error });
        }
    }
    Response::SendKeys(SendKeysResponse { key_count })
}

pub(in crate::handler) async fn write_bytes_to_target_io(
    write: PaneInputWrite,
    bytes: Vec<u8>,
) -> Result<(), RmuxError> {
    write_bytes_to_target_io_classified(write, bytes)
        .await
        .map_err(PaneInputWriteFailure::into_error)
}

async fn write_bytes_to_target_io_classified(
    write: PaneInputWrite,
    bytes: Vec<u8>,
) -> Result<(), PaneInputWriteFailure> {
    if bytes.is_empty() {
        return Ok(());
    }
    let PaneInputWrite {
        session_name,
        window_index,
        pane_index,
        sink,
    } = write;
    match sink {
        PaneInputSink::Disabled => Ok(()),
        #[cfg(windows)]
        PaneInputSink::QueuedStarting => Ok(()),
        #[cfg(windows)]
        PaneInputSink::ConsoleUtf8(pid) => {
            tokio::task::spawn_blocking(move || rmux_pty::write_windows_console_utf8(pid, &bytes))
                .await
                .map_err(|error| {
                    PaneInputWriteFailure::other(RmuxError::Server(format!(
                        "pane console text task failed: {error}"
                    )))
                })?
                .map_err(|error| {
                    PaneInputWriteFailure::from_console(
                        &error,
                        &session_name,
                        window_index,
                        pane_index,
                    )
                })
        }
        #[cfg(windows)]
        PaneInputSink::OrderedConsoleUtf8 { master, pid } => {
            tokio::task::spawn_blocking(move || {
                master.write_windows_console_utf8_ordered(
                    pid,
                    &bytes,
                    PANE_INPUT_WRITE_TIMEOUT.saturating_add(PANE_INPUT_WRITE_RECOVERY_GRACE),
                )
            })
            .await
            .map_err(|error| {
                PaneInputWriteFailure::other(RmuxError::Server(format!(
                    "pane ordered console text task failed: {error}"
                )))
            })?
            .map_err(|error| {
                PaneInputWriteFailure::from_console(&error, &session_name, window_index, pane_index)
            })
        }
        PaneInputSink::Pty(master) => match write_pane_bytes(master, bytes).await {
            Ok(()) => Ok(()),
            Err(error) => Err(PaneInputWriteFailure::from_pty(
                error,
                &session_name,
                window_index,
                pane_index,
            )),
        },
        #[cfg(test)]
        PaneInputSink::CapturedForTest => Ok(()),
    }
}

pub(in crate::handler) async fn write_attached_bytes_to_target_io(
    write: PaneInputWrite,
    bytes: Vec<u8>,
) -> Result<(), RmuxError> {
    match write_bytes_to_target_io_classified(write, bytes).await {
        Ok(()) => Ok(()),
        Err(failure) => failure.into_attached_result(),
    }
}

#[cfg(windows)]
pub(in crate::handler::pane_support) async fn write_windows_console_input_action_to_target_io(
    write: PaneConsoleInputWrite,
    action: WindowsConsoleInputAction,
) -> Result<(), RmuxError> {
    write_windows_console_input_action_to_target_io_classified(write, action)
        .await
        .map_err(PaneInputWriteFailure::into_error)
}

#[cfg(windows)]
async fn write_windows_console_input_action_to_target_io_classified(
    write: PaneConsoleInputWrite,
    action: WindowsConsoleInputAction,
) -> Result<(), PaneInputWriteFailure> {
    let PaneConsoleInputWrite {
        session_name,
        window_index,
        pane_index,
        sink,
    } = write;
    match sink {
        PaneConsoleInputSink::Disabled => Ok(()),
        PaneConsoleInputSink::QueuedStarting => Ok(()),
        PaneConsoleInputSink::ConsolePid(pid) => {
            trace_windows_console_input(
                &session_name,
                window_index,
                pane_index,
                pid,
                action,
                "dispatch",
            );
            tokio::task::spawn_blocking(move || match action {
                WindowsConsoleInputAction::Key(key) => {
                    crate::windows_console_input::write_with_transient_retry(|| {
                        rmux_pty::write_windows_console_key(pid, key)
                    })
                }
                WindowsConsoleInputAction::KeyThenInterrupt(key) => {
                    crate::windows_console_input::write_console_key_then_processed_interrupt(
                        || rmux_pty::write_windows_console_key_reporting_processed_input(pid, key),
                        || rmux_pty::send_windows_console_interrupt(pid),
                    )
                }
                WindowsConsoleInputAction::Interrupt => {
                    crate::windows_console_input::write_with_transient_retry(|| {
                        rmux_pty::send_windows_console_interrupt(pid)
                    })
                }
            })
            .await
            .map_err(|error| {
                PaneInputWriteFailure::other(RmuxError::Server(format!(
                    "pane console input task failed: {error}"
                )))
            })?
            .map_err(|error| {
                PaneInputWriteFailure::from_console(&error, &session_name, window_index, pane_index)
            })
        }
        #[cfg(test)]
        PaneConsoleInputSink::CapturedForTest => Ok(()),
    }
}

/// Console-key counterpart of [`write_attached_bytes_to_target_io`].
#[cfg(windows)]
pub(in crate::handler::pane_support) async fn write_attached_windows_console_input_action_to_target_io(
    write: PaneConsoleInputWrite,
    action: WindowsConsoleInputAction,
) -> Result<(), RmuxError> {
    match write_windows_console_input_action_to_target_io_classified(write, action).await {
        Ok(()) => Ok(()),
        Err(failure) => failure.into_attached_result(),
    }
}

#[cfg(windows)]
fn trace_windows_console_input(
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
    pid: ProcessId,
    action: WindowsConsoleInputAction,
    stage: &'static str,
) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        %session_name,
        window_index,
        pane_index,
        pid = pid.as_u32(),
        ?action,
        stage,
        "Windows console input action"
    );
}

/// Unix pane input joins the PTY's ordered input queue, as tmux appends it to
/// the pane's write buffer: a program that is busy and not reading yet delays
/// the input but neither truncates it nor fails the writer, and the caller
/// (attached client, paste-buffer, send-keys) is not held while it waits.
#[cfg(unix)]
async fn write_pane_bytes(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    master.queue_input(&bytes)
}

#[cfg(windows)]
async fn write_pane_bytes(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        master.write_all_with_stall_recovery(
            &bytes,
            PANE_INPUT_WRITE_TIMEOUT,
            PANE_INPUT_WRITE_RECOVERY_GRACE,
        )
    })
    .await
    .map_err(|error| std::io::Error::other(format!("pane write task failed: {error}")))?
}

#[cfg(not(any(unix, windows)))]
async fn write_pane_bytes(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    master.write_all(&bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use rmux_proto::SessionName;
    use rmux_pty::{ChildCommand, PtyMaster, SpawnedPty, TerminalSize};

    use super::{write_attached_bytes_to_target_io, write_bytes_to_target_io};
    use crate::handler::pane_support::pane_io_encoding::{PaneInputSink, PaneInputWrite};

    /// The 400-line, 20 400-byte paste of R3512 H1: far more than a PTY input
    /// buffer holds while the program on the slave is not reading.
    fn busy_paste() -> Vec<u8> {
        (1..=400)
            .map(|line| format!("echo line-{line:03}-{}\n", "x".repeat(36)))
            .collect::<String>()
            .into_bytes()
    }

    fn pty_write(master: PtyMaster) -> PaneInputWrite {
        PaneInputWrite {
            session_name: SessionName::new("busy").expect("valid session name"),
            window_index: 0,
            pane_index: 0,
            sink: PaneInputSink::Pty(master),
        }
    }

    /// Tests run in parallel and the clock may not tell two calls apart, so
    /// a per-process counter keeps every pane's output file distinct.
    fn unique_output(label: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rmux-busy-pane-{label}-{}-{serial}-{unique}",
            std::process::id()
        ))
    }

    /// A canonical-mode pane that does not read for `busy`, then reads exactly
    /// `expected` bytes into `output` and prints DONE.
    fn spawn_busy_reader(
        busy: &str,
        expected: usize,
        output: &Path,
    ) -> Result<SpawnedPty, Box<dyn Error>> {
        let script = format!(
            "stty -echo; printf READY; sleep {busy}; head -c {expected} > \"$RMUX_TEST_OUT\"; printf DONE; sleep 30"
        );
        let spawned = ChildCommand::new("/bin/sh")
            .args(["-c", &script])
            .env("RMUX_TEST_OUT", output)
            .size(TerminalSize::new(120, 30))
            .spawn()?;
        wait_for_output(spawned.master(), "READY", Duration::from_secs(5))?;
        Ok(spawned)
    }

    fn wait_for_output(
        master: &PtyMaster,
        marker: &str,
        timeout: Duration,
    ) -> Result<(), Box<dyn Error>> {
        let io = master.try_clone_io()?;
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        let mut buffer = [0_u8; 4096];
        while !String::from_utf8_lossy(&seen).contains(marker) {
            if Instant::now() >= deadline {
                return Err(format!(
                    "pane did not print {marker:?} within {timeout:?}; saw {:?}",
                    String::from_utf8_lossy(&seen)
                )
                .into());
            }
            match io.try_read(&mut buffer) {
                Ok(0) => return Err("pane closed before its marker".into()),
                Ok(read) => seen.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn stop(mut spawned: SpawnedPty, output: &Path) {
        let _ = spawned.child().terminate_forcefully();
        let _ = spawned.child_mut().wait();
        let _ = std::fs::remove_file(output);
    }

    /// R3512 H1: input for a pane that stays busy longer than the old 2 s
    /// no-progress deadline arrives whole, without failing the writer, and the
    /// writer is not held while the pane is busy; a second pane written during
    /// that wait is served at once.
    async fn assert_busy_pane_receives_whole_paste(attached: bool) -> Result<(), Box<dyn Error>> {
        let paste = busy_paste();
        let busy_output = unique_output(if attached { "attached" } else { "command" });
        let busy = spawn_busy_reader("3", paste.len(), &busy_output)?;
        let other_output = unique_output("other");
        let other = spawn_busy_reader("0", 6, &other_output)?;

        let started = Instant::now();
        let write = pty_write(busy.master().try_clone()?);
        let result = if attached {
            write_attached_bytes_to_target_io(write, paste.clone()).await
        } else {
            write_bytes_to_target_io(write, paste.clone()).await
        };
        let returned_after = started.elapsed();
        let other_result =
            write_bytes_to_target_io(pty_write(other.master().try_clone()?), b"hello\n".to_vec())
                .await;
        let other_served = wait_for_output(other.master(), "DONE", Duration::from_secs(1));
        let other_served_after = started.elapsed();
        let delivered = wait_for_output(busy.master(), "DONE", Duration::from_secs(15));
        let received = std::fs::read(&busy_output).unwrap_or_default();
        let other_received = std::fs::read(&other_output).unwrap_or_default();
        stop(busy, &busy_output);
        stop(other, &other_output);

        assert!(result.is_ok(), "busy pane write failed: {result:?}");
        assert!(
            returned_after < Duration::from_secs(1),
            "busy pane write held its caller for {returned_after:?}"
        );
        assert!(
            other_result.is_ok(),
            "second pane write failed: {other_result:?}"
        );
        assert!(
            other_served.is_ok() && other_served_after < Duration::from_secs(2),
            "second pane not served while the first was busy: {other_served:?} \
             after {other_served_after:?}"
        );
        assert_eq!(other_received, b"hello\n");
        delivered?;
        assert_eq!(
            received.len(),
            paste.len(),
            "busy pane received {} of {} bytes",
            received.len(),
            paste.len()
        );
        assert!(
            received == paste,
            "busy pane received the paste out of order"
        );
        Ok(())
    }

    #[tokio::test]
    async fn command_input_to_busy_pane_arrives_whole_without_holding_the_server(
    ) -> Result<(), Box<dyn Error>> {
        assert_busy_pane_receives_whole_paste(false).await
    }

    #[tokio::test]
    async fn attached_input_to_busy_pane_arrives_whole_and_keeps_the_attach(
    ) -> Result<(), Box<dyn Error>> {
        assert_busy_pane_receives_whole_paste(true).await
    }
}
