//! Blocking tmux-compatible control-mode client transport.

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;

use rmux_ipc::BlockingLocalStream;
#[cfg(any(test, windows))]
use rmux_proto::CONTROL_STDIN_EOF_MARKER;
use rmux_proto::{ControlMode, CONTROL_CONTROL_END, CONTROL_CONTROL_START};
#[cfg(any(test, windows))]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(any(test, windows))]
use tokio::sync::mpsc as tokio_mpsc;

use crate::{connection::ControlModeUpgrade, ClientError};

mod handshake;
mod output;
mod records;

use output::ControlOutputEnd;
#[cfg(any(test, windows))]
use output::ControlOutputProgress;

/// Exit status of a control-mode session that ended with its `%exit` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlModeStatus {
    /// tmux exits with status 0.
    Success,
    /// A command failed in a client that never joined a session, so the
    /// server ended it once its command-line commands had run; tmux 3.7b
    /// exits with status 1 (as for `-C attach -t missing`).
    CommandFailed,
    /// Explicit process exit code from the negotiated server transport.
    ExitCode(i32),
}

/// Drives a control-mode session using the process stdio streams.
///
/// Returns the session's [`ControlModeStatus`] once the server has written
/// its terminal `%exit` record. Returns [`ClientError::UnexpectedEof`] when the
/// server closed the control stream without one, as when the server process
/// dies. Like tmux, the client has then already ended its output with
/// `%exit server exited unexpectedly` (and the `-CC` terminator), so callers
/// should exit with status 1 without printing a second diagnostic.
pub fn drive_control_mode(
    upgrade: ControlModeUpgrade,
    initial_commands: &[String],
) -> Result<ControlModeStatus, ClientError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    drive_control_mode_with_stdio(upgrade, initial_commands, stdin, stdout)
}

/// Drives a control-mode session using explicit input and output streams.
///
/// See [`drive_control_mode`] for the lost-server contract.
pub fn drive_control_mode_with_stdio<R, W>(
    upgrade: ControlModeUpgrade,
    initial_commands: &[String],
    input: R,
    mut output: W,
) -> Result<ControlModeStatus, ClientError>
where
    R: Read + Send + 'static,
    W: Write + Send,
{
    let mode = upgrade.mode();
    if mode.is_control_control() {
        output
            .write_all(CONTROL_CONTROL_START.as_bytes())
            .map_err(ClientError::Io)?;
        output.flush().map_err(ClientError::Io)?;
    }

    let records = upgrade.records;
    let stream = upgrade.into_stream();
    let end = drive_control_stream(stream, initial_commands, input, &mut output, records)?;
    if output_needs_suffix(mode) {
        output
            .write_all(CONTROL_CONTROL_END.as_bytes())
            .map_err(ClientError::Io)?;
        output.flush().map_err(ClientError::Io)?;
    }

    match end {
        ControlOutputEnd::ExitDelivered(status) => Ok(status),
        ControlOutputEnd::ServerLost => Err(ClientError::UnexpectedEof),
    }
}

#[cfg(unix)]
fn drive_control_stream<R, W>(
    stream: BlockingLocalStream,
    initial_commands: &[String],
    mut input: R,
    output: &mut W,
    records: bool,
) -> Result<ControlOutputEnd, ClientError>
where
    R: Read + Send + 'static,
    W: Write + Send,
{
    write_initial_commands(&stream, initial_commands)?;
    ensure_blocking(&stream).map_err(ClientError::Io)?;
    let mut writer = stream.try_clone().map_err(ClientError::Io)?;
    let (stdin_done_tx, stdin_done_rx) = mpsc::sync_channel(1);
    let stdin_thread = thread::spawn(move || {
        let result = io::copy(&mut input, &mut writer).map(|_| ());
        let _ = shutdown_write(&writer);
        let _ = stdin_done_tx.send(result);
    });

    let copy_result =
        output::copy_control_output_with_records(stream, output, records).map_err(ClientError::Io);
    let stdin_result = poll_input_thread(&stdin_done_rx)?;
    if stdin_result.is_some() {
        stdin_thread
            .join()
            .map_err(|_| ClientError::Io(io::Error::other("control input thread panicked")))?;
    }

    let end = copy_result?;
    if let Some(stdin_result) = stdin_result {
        finish_control_input_after_output_closed(stdin_result).map_err(ClientError::Io)?;
    }
    Ok(end)
}

#[cfg(windows)]
const CONTROL_STDIN_QUEUE_CAPACITY: usize = 256;
#[cfg(windows)]
const CONTROL_STDOUT_QUEUE_CAPACITY: usize = 256;

#[cfg(windows)]
fn drive_control_stream<R, W>(
    stream: BlockingLocalStream,
    initial_commands: &[String],
    input: R,
    output: &mut W,
    records: bool,
) -> Result<ControlOutputEnd, ClientError>
where
    R: Read + Send + 'static,
    W: Write + Send,
{
    let (input_tx, input_rx) = tokio_mpsc::channel(CONTROL_STDIN_QUEUE_CAPACITY);
    let (output_tx, output_rx) = tokio_mpsc::channel(CONTROL_STDOUT_QUEUE_CAPACITY);
    let (stdin_done_tx, stdin_done_rx) = mpsc::sync_channel(1);
    let stdin_thread = thread::spawn(move || {
        let result = copy_control_input(input, input_tx);
        let _ = stdin_done_tx.send(result);
    });

    let (pipe, runtime) = stream.into_async_parts();
    let copy_result: Result<ControlOutputEnd, ClientError> = thread::scope(|scope| {
        let output_thread = scope.spawn(move || write_queued_control_output(output, output_rx));
        let copy_result = runtime
            .block_on(drive_async_control_with_records(
                pipe,
                initial_commands,
                input_rx,
                output_tx,
                records,
            ))
            .map_err(ClientError::Io);
        let output_result = output_thread
            .join()
            .map_err(|_| ClientError::Io(io::Error::other("control output thread panicked")))?;

        let end = copy_result?;
        output_result.map_err(ClientError::Io)?;
        Ok(end)
    });
    let stdin_result = poll_input_thread(&stdin_done_rx)?;

    if stdin_result.is_some() {
        stdin_thread
            .join()
            .map_err(|_| ClientError::Io(io::Error::other("control input thread panicked")))?;
    }

    let end = copy_result?;
    if let Some(stdin_result) = stdin_result {
        finish_control_input_after_output_closed(stdin_result).map_err(ClientError::Io)?;
    }
    Ok(end)
}

fn output_needs_suffix(mode: ControlMode) -> bool {
    mode.is_control_control()
}

fn finish_control_input_after_output_closed(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::NotConnected
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

fn poll_input_thread(
    stdin_done_rx: &mpsc::Receiver<io::Result<()>>,
) -> Result<Option<io::Result<()>>, ClientError> {
    match stdin_done_rx.try_recv() {
        Ok(result) => Ok(Some(result)),
        Err(mpsc::TryRecvError::Empty) => Ok(None),
        Err(mpsc::TryRecvError::Disconnected) => Err(ClientError::Io(io::Error::other(
            "control input thread terminated unexpectedly",
        ))),
    }
}

#[cfg(unix)]
fn write_initial_commands(
    stream: &BlockingLocalStream,
    initial_commands: &[String],
) -> Result<(), ClientError> {
    if initial_commands.is_empty() {
        return Ok(());
    }

    let mut writer = stream.try_clone().map_err(ClientError::Io)?;
    for command in initial_commands {
        writer
            .write_all(command.as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .map_err(ClientError::Io)?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_blocking(stream: &BlockingLocalStream) -> io::Result<()> {
    stream.set_nonblocking(false)
}

#[cfg(unix)]
fn shutdown_write(stream: &BlockingLocalStream) -> io::Result<()> {
    stream.shutdown(std::net::Shutdown::Write)
}

#[cfg(windows)]
fn copy_control_input<R>(mut input: R, input_tx: tokio_mpsc::Sender<Vec<u8>>) -> io::Result<()>
where
    R: Read,
{
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes_read = match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };

        if input_tx
            .blocking_send(buffer[..bytes_read].to_vec())
            .is_err()
        {
            return Ok(());
        }
    }
}

#[cfg(any(test, windows))]
async fn drive_async_control_with_records<Stream>(
    stream: Stream,
    initial_commands: &[String],
    mut input_rx: tokio_mpsc::Receiver<Vec<u8>>,
    output_tx: tokio_mpsc::Sender<Vec<u8>>,
    records: bool,
) -> io::Result<ControlOutputEnd>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    let mut input_closed = false;
    let mut progress = ControlOutputProgress::new(records);
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_async_initial_commands(&mut writer, initial_commands).await?;
    let mut buffer = [0_u8; 8192];

    loop {
        tokio::select! {
            input = input_rx.recv(), if !input_closed => {
                match input {
                    Some(bytes) => {
                        writer.write_all(&bytes).await?;
                    }
                    None => {
                        writer.write_all(CONTROL_STDIN_EOF_MARKER.as_bytes()).await?;
                        writer.write_all(b"\n").await?;
                        writer.flush().await?;
                        writer.shutdown().await?;
                        input_closed = true;
                    }
                }
            }
            bytes_read = reader.read(&mut buffer) => {
                let bytes_read = match bytes_read {
                    Ok(bytes_read) => bytes_read,
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
                    Err(error) => return Err(error),
                };
                if bytes_read == 0 {
                    return finish_closed_async_control(&progress, &output_tx).await;
                }
                let text = progress.decode(&buffer[..bytes_read])?;
                send_control_output(&output_tx, &text).await?;
            }
        }
    }
}

#[cfg(any(test, windows))]
async fn finish_closed_async_control(
    progress: &ControlOutputProgress,
    output_tx: &tokio_mpsc::Sender<Vec<u8>>,
) -> io::Result<ControlOutputEnd> {
    if let Some(suffix) = progress.lost_server_suffix() {
        send_control_output(output_tx, &suffix).await?;
    }
    Ok(progress.closed_end())
}

#[cfg(windows)]
fn write_queued_control_output<W>(
    output: &mut W,
    mut output_rx: tokio_mpsc::Receiver<Vec<u8>>,
) -> io::Result<()>
where
    W: Write,
{
    while let Some(bytes) = output_rx.blocking_recv() {
        output.write_all(&bytes)?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(any(test, windows))]
async fn send_control_output(
    output_tx: &tokio_mpsc::Sender<Vec<u8>>,
    bytes: &[u8],
) -> io::Result<()> {
    output_tx
        .send(bytes.to_vec())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control output writer stopped"))
}

#[cfg(any(test, windows))]
async fn write_async_initial_commands<Writer>(
    writer: &mut Writer,
    initial_commands: &[String],
) -> io::Result<()>
where
    Writer: AsyncWrite + Unpin,
{
    for command in initial_commands {
        writer.write_all(command.as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }
    writer.flush().await?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{self, Cursor, Read, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    use rmux_proto::{
        ClientTerminalContext, ControlMode, ControlModeResponse, MAX_INITIAL_CONTROL_COMMANDS,
    };

    use super::{drive_control_mode_with_stdio, finish_control_input_after_output_closed};
    use crate::connection::{Connection, ControlModeUpgrade};

    #[test]
    fn excessive_initial_commands_are_rejected_before_any_stream_write() {
        let (client, mut server) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let connection = Connection::new(client).expect("client connection");
        let commands = vec![String::new(); MAX_INITIAL_CONTROL_COMMANDS + 1];

        let error = connection
            .begin_control_mode_with_initial_commands(
                ControlMode::Plain,
                ClientTerminalContext::default(),
                &commands,
            )
            .expect_err("oversized command batch must fail locally");

        assert!(
            error
                .to_string()
                .contains("too many initial control-mode commands"),
            "unexpected error: {error}"
        );
        let mut byte = [0_u8; 1];
        assert_eq!(
            server.read(&mut byte).expect("read closed client stream"),
            0,
            "client must not write a partial upgrade before rejecting the batch"
        );
    }

    #[test]
    fn closed_control_transport_supersedes_input_side_connection_errors() {
        for kind in [
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::NotConnected,
        ] {
            finish_control_input_after_output_closed(Err(io::Error::from(kind)))
                .expect("closed server transport makes further control input irrelevant");
        }

        let error = finish_control_input_after_output_closed(Err(io::Error::from(
            io::ErrorKind::InvalidData,
        )))
        .expect_err("unrelated input failures remain visible");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn control_control_mode_wraps_output_with_dcs_sequences() {
        let (left, right) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let writer = std::thread::spawn(move || {
            let mut right = right;
            right.write_all(b"%exit\n").expect("write output");
        });

        let mut output = Vec::new();
        drive_control_mode_with_stdio(
            ControlModeUpgrade {
                records: false,
                response: ControlModeResponse {
                    mode: ControlMode::ControlControl,
                },
                stream: left,
            },
            &[],
            Cursor::new(Vec::<u8>::new()),
            &mut output,
        )
        .expect("control mode succeeds");
        writer.join().expect("writer thread");

        let rendered = String::from_utf8(output).expect("utf8");
        assert!(rendered.starts_with(rmux_proto::CONTROL_CONTROL_START));
        assert!(rendered.contains("%exit\n"));
        assert!(rendered.ends_with(rmux_proto::CONTROL_CONTROL_END));
    }

    fn drive_until_transport_closes(
        mode: ControlMode,
        transcript: &'static [u8],
    ) -> (Result<super::ControlModeStatus, crate::ClientError>, String) {
        let (left, right) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let (input_reader, input_writer) =
            std::os::unix::net::UnixStream::pair().expect("input socket pair");
        let server = std::thread::spawn(move || {
            let mut right = right;
            right.write_all(transcript).expect("write transcript");
        });
        let mut output = Vec::new();
        let result = drive_control_mode_with_stdio(
            ControlModeUpgrade {
                records: false,
                response: ControlModeResponse { mode },
                stream: left,
            },
            &[],
            input_reader,
            &mut output,
        );
        drop(input_writer);
        server.join().expect("server thread");
        (result, String::from_utf8(output).expect("utf8"))
    }

    #[test]
    fn plain_control_mode_reports_a_lost_server_like_tmux() {
        let (result, output) = drive_until_transport_closes(ControlMode::Plain, b"%output %0 $ \n");

        assert!(
            matches!(result, Err(crate::ClientError::UnexpectedEof)),
            "a server that vanished without %exit is a failure: {result:?}"
        );
        assert_eq!(output, "%output %0 $ \n%exit server exited unexpectedly\n");
    }

    #[test]
    fn control_control_mode_reports_a_lost_server_inside_the_dcs_frame() {
        let (result, output) =
            drive_until_transport_closes(ControlMode::ControlControl, b"%output %0 $ \n");

        assert!(
            matches!(result, Err(crate::ClientError::UnexpectedEof)),
            "a server that vanished without %exit is a failure: {result:?}"
        );
        assert_eq!(
            output,
            format!(
                "{}%output %0 $ \n%exit server exited unexpectedly\n{}",
                rmux_proto::CONTROL_CONTROL_START,
                rmux_proto::CONTROL_CONTROL_END
            )
        );
    }

    #[test]
    fn control_mode_with_a_server_exit_record_still_succeeds() {
        let (result, output) =
            drive_until_transport_closes(ControlMode::Plain, b"%output %0 $ \n%exit\n");

        result.expect("server-delivered %exit is a clean end");
        assert_eq!(output, "%output %0 $ \n%exit\n");
    }

    #[test]
    fn control_mode_returns_after_server_exit_without_waiting_for_input_eof() {
        let (left, right) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let (input_reader, input_writer) =
            std::os::unix::net::UnixStream::pair().expect("input socket pair");
        let server = std::thread::spawn(move || {
            let mut right = right;
            right.write_all(b"%exit\n").expect("write exit");
        });
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut output = Vec::new();
            let result = drive_control_mode_with_stdio(
                ControlModeUpgrade {
                    records: false,
                    response: ControlModeResponse {
                        mode: ControlMode::Plain,
                    },
                    stream: left,
                },
                &[],
                input_reader,
                &mut output,
            );
            done_tx
                .send((result, output))
                .expect("report control mode result");
        });

        let done = done_rx.recv_timeout(Duration::from_secs(1));
        drop(input_writer);
        worker.join().expect("worker thread");
        server.join().expect("server thread");

        let (result, output) = done.expect("control mode should exit promptly");
        result.expect("control mode succeeds");
        assert_eq!(String::from_utf8(output).expect("utf8"), "%exit\n");
    }
}

#[cfg(test)]
#[path = "control/windows_tests.rs"]
mod windows_tests;

#[cfg(test)]
async fn drive_async_control<Stream: AsyncRead + AsyncWrite + Unpin>(
    stream: Stream,
    initial_commands: &[String],
    input_rx: tokio_mpsc::Receiver<Vec<u8>>,
    output_tx: tokio_mpsc::Sender<Vec<u8>>,
) -> io::Result<ControlOutputEnd> {
    drive_async_control_with_records(stream, initial_commands, input_rx, output_tx, false).await
}
