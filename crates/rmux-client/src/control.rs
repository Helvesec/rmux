//! Blocking tmux-compatible control-mode client transport.

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;

use rmux_ipc::BlockingLocalStream;
#[cfg(any(test, windows))]
use rmux_proto::CONTROL_STDIN_EOF_MARKER;
use rmux_proto::{
    ClientTerminalContext, ControlMode, ControlModeRequest, Request, Response, CONTROL_CONTROL_END,
    CONTROL_CONTROL_START,
};
#[cfg(any(test, windows))]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(any(test, windows))]
use tokio::sync::mpsc as tokio_mpsc;

use crate::{
    connection::{read_response_frame_exact, Connection, ControlModeUpgrade, ControlTransition},
    ClientError,
};

impl Connection {
    /// Requests a control-mode upgrade and, on success, yields the raw local
    /// stream for tmux-compatible text control traffic.
    pub fn begin_control_mode(
        self,
        mode: ControlMode,
        client_terminal: ClientTerminalContext,
    ) -> Result<ControlTransition, ClientError> {
        self.begin_control_mode_with_initial_commands(mode, client_terminal, &[])
    }

    /// Requests a control-mode upgrade and writes command-line commands across
    /// the upgrade boundary so the server can frame them as tmux argv commands.
    pub fn begin_control_mode_with_initial_commands(
        mut self,
        mode: ControlMode,
        client_terminal: ClientTerminalContext,
        initial_commands: &[String],
    ) -> Result<ControlTransition, ClientError> {
        let initial_command_count = u32::try_from(initial_commands.len()).map_err(|_| {
            ClientError::Protocol(rmux_proto::RmuxError::Server(
                "too many initial control-mode commands".to_owned(),
            ))
        })?;
        self.write_request(&Request::ControlMode(ControlModeRequest {
            mode,
            client_terminal,
            initial_command_count,
        }))?;
        write_initial_control_commands(self.stream_mut(), initial_commands)?;
        let response = read_response_frame_exact(self.stream_mut())?;

        match response {
            Response::ControlMode(response) => Ok(ControlTransition::Upgraded(
                self.into_control_upgrade(response)?,
            )),
            other => Ok(ControlTransition::Rejected(other)),
        }
    }
}

fn write_initial_control_commands<W>(
    stream: &mut W,
    initial_commands: &[String],
) -> Result<(), ClientError>
where
    W: Write,
{
    for command in initial_commands {
        stream
            .write_all(command.as_bytes())
            .map_err(ClientError::Io)?;
        stream.write_all(b"\n").map_err(ClientError::Io)?;
    }
    Ok(())
}

/// Drives a control-mode session using the process stdio streams.
pub fn drive_control_mode(
    upgrade: ControlModeUpgrade,
    initial_commands: &[String],
) -> Result<(), ClientError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    drive_control_mode_with_stdio(upgrade, initial_commands, stdin, stdout)
}

/// Drives a control-mode session using explicit input and output streams.
pub fn drive_control_mode_with_stdio<R, W>(
    upgrade: ControlModeUpgrade,
    initial_commands: &[String],
    input: R,
    mut output: W,
) -> Result<(), ClientError>
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

    let stream = upgrade.into_stream();
    let copy_result = drive_control_stream(stream, initial_commands, input, &mut output);
    if copy_result.is_ok() && output_needs_suffix(mode) {
        output
            .write_all(CONTROL_CONTROL_END.as_bytes())
            .map_err(ClientError::Io)?;
        output.flush().map_err(ClientError::Io)?;
    }

    copy_result
}

#[cfg(unix)]
fn drive_control_stream<R, W>(
    stream: BlockingLocalStream,
    initial_commands: &[String],
    mut input: R,
    output: &mut W,
) -> Result<(), ClientError>
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

    let copy_result = copy_control_output(stream, output).map_err(ClientError::Io);
    let stdin_result = poll_input_thread(&stdin_done_rx)?;
    if stdin_result.is_some() {
        stdin_thread
            .join()
            .map_err(|_| ClientError::Io(io::Error::other("control input thread panicked")))?;
    }

    copy_result?;
    if let Some(stdin_result) = stdin_result {
        stdin_result.map_err(ClientError::Io)?;
    }
    Ok(())
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
) -> Result<(), ClientError>
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
    let copy_result = thread::scope(|scope| {
        let output_thread = scope.spawn(move || write_queued_control_output(output, output_rx));
        let copy_result = runtime
            .block_on(drive_async_control(
                pipe,
                initial_commands,
                input_rx,
                output_tx,
            ))
            .map_err(ClientError::Io);
        let output_result = output_thread
            .join()
            .map_err(|_| ClientError::Io(io::Error::other("control output thread panicked")))?;

        copy_result?;
        output_result.map_err(ClientError::Io)
    });
    let stdin_result = poll_input_thread(&stdin_done_rx)?;

    if stdin_result.is_some() {
        stdin_thread
            .join()
            .map_err(|_| ClientError::Io(io::Error::other("control input thread panicked")))?;
    }

    copy_result?;
    if let Some(stdin_result) = stdin_result {
        stdin_result.map_err(ClientError::Io)?;
    }
    Ok(())
}

fn output_needs_suffix(mode: ControlMode) -> bool {
    mode.is_control_control()
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
fn copy_control_output(mut stream: BlockingLocalStream, output: &mut impl Write) -> io::Result<()> {
    let mut buffer = [0_u8; 8192];

    loop {
        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(());
        }
        output.write_all(&buffer[..bytes_read])?;
        output.flush()?;
    }
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
async fn drive_async_control<Stream>(
    stream: Stream,
    initial_commands: &[String],
    mut input_rx: tokio_mpsc::Receiver<Vec<u8>>,
    output_tx: tokio_mpsc::Sender<Vec<u8>>,
) -> io::Result<()>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    let mut input_closed = false;
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
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
                    Err(error) => return Err(error),
                };
                if bytes_read == 0 {
                    return Ok(());
                }
                send_control_output(&output_tx, &buffer[..bytes_read]).await?;
            }
        }
    }
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
    use std::io::{Cursor, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    use rmux_proto::{ControlMode, ControlModeResponse};

    use super::drive_control_mode_with_stdio;
    use crate::connection::ControlModeUpgrade;

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
