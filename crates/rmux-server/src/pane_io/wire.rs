use std::future::pending;
use std::io;

use rmux_core::events::OutputCursorItem;
use rmux_ipc::{is_peer_disconnect, LocalStream};
use rmux_proto::{encode_attach_message, AttachFrameDecoder, AttachMessage};
#[cfg(unix)]
use rmux_pty::PtyIo;
#[cfg(unix)]
use rmux_pty::PtyMaster;
#[cfg(unix)]
use tokio::io::unix::AsyncFd;
use tracing::warn;

use crate::outer_terminal::OuterTerminal;

use super::types::{AttachTarget, OpenAttachTarget, PaneOutputReceiver};

pub(super) fn open_attach_target(target: AttachTarget) -> io::Result<OpenAttachTarget> {
    let AttachTarget {
        session_name,
        pane_master,
        pane_output,
        render_frame,
        outer_terminal,
        cursor_style,
        persistent_overlay_state_id,
        live_pane,
    } = target;
    Ok(OpenAttachTarget {
        session_name,
        _pane_master: pane_master,
        pane_output: Some(pane_output.subscribe()),
        render_frame,
        outer_terminal,
        cursor_style,
        persistent_overlay_state_id,
        live_pane,
    })
}

#[cfg(unix)]
pub(super) fn open_pane_writer(pane_master: PtyMaster) -> io::Result<AsyncFd<PtyIo>> {
    let pane_writer = pane_master.into_io();
    pane_writer.set_nonblocking()?;

    AsyncFd::new(pane_writer)
}

pub(super) async fn emit_render_frame(
    stream: &LocalStream,
    outer_terminal: &OuterTerminal,
    render_frame: &[u8],
) -> io::Result<()> {
    let frame = outer_terminal.wrap_render_frame(render_frame);
    emit_attach_bytes(stream, &frame).await
}

pub(super) async fn read_socket_bytes(
    stream: &LocalStream,
    decoder: &mut AttachFrameDecoder,
    buffer: &mut [u8],
) -> io::Result<bool> {
    loop {
        stream.readable().await?;
        match stream.try_read(buffer) {
            Ok(0) => return Ok(false),
            Ok(bytes_read) => {
                decoder.push_bytes(&buffer[..bytes_read]);
                return Ok(true);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        }
    }
}

pub(super) enum TrySocketRead {
    Read,
    Closed,
    WouldBlock,
}

pub(super) fn try_read_socket_bytes(
    stream: &LocalStream,
    decoder: &mut AttachFrameDecoder,
    buffer: &mut [u8],
) -> io::Result<TrySocketRead> {
    match stream.try_read(buffer) {
        Ok(0) => Ok(TrySocketRead::Closed),
        Ok(bytes_read) => {
            decoder.push_bytes(&buffer[..bytes_read]);
            Ok(TrySocketRead::Read)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(TrySocketRead::WouldBlock),
        Err(error) => Err(error),
    }
}

pub(super) async fn emit_attach_message(
    stream: &LocalStream,
    message: &AttachMessage,
) -> io::Result<()> {
    let frame = encode_attach_message(message).map_err(io::Error::other)?;
    emit_attach_bytes(stream, &frame).await
}

pub(super) async fn emit_attach_frame(
    stream: &LocalStream,
    message: &AttachMessage,
) -> io::Result<()> {
    let frame = encode_attach_message(message).map_err(io::Error::other)?;
    write_all_to_stream(stream, &frame).await
}

pub(super) async fn recv_pane_output(
    pane_output: &mut PaneOutputReceiver,
) -> io::Result<OutputCursorItem> {
    match pane_output.recv().await {
        OutputCursorItem::Event(event) => Ok(OutputCursorItem::Event(event)),
        OutputCursorItem::Gap(gap) => {
            warn!(
                expected_sequence = gap.expected_sequence(),
                resume_sequence = gap.resume_sequence(),
                missed_events = gap.missed_events(),
                recent_bytes = gap.recent_snapshot().len(),
                "attach pane output receiver lagged"
            );
            Ok(OutputCursorItem::Gap(gap))
        }
    }
}

pub(super) async fn recv_pane_output_optional(
    pane_output: Option<&mut PaneOutputReceiver>,
) -> io::Result<Option<OutputCursorItem>> {
    match pane_output {
        Some(pane_output) => recv_pane_output(pane_output).await.map(Some),
        None => pending().await,
    }
}

pub(super) async fn emit_attach_data_frame(stream: &LocalStream, bytes: &[u8]) -> io::Result<()> {
    let frame =
        encode_attach_message(&AttachMessage::Data(bytes.to_vec())).map_err(io::Error::other)?;
    write_all_to_stream(stream, &frame).await
}

pub(super) async fn emit_attach_bytes(stream: &LocalStream, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }

    emit_attach_data_frame(stream, bytes).await
}

pub(super) async fn emit_attach_stop(
    stream: &LocalStream,
    current_target: &OpenAttachTarget,
) -> io::Result<()> {
    emit_attach_bytes(
        stream,
        &current_target.outer_terminal.attach_stop_sequence(),
    )
    .await
}

pub(super) async fn emit_detached_message(
    stream: &LocalStream,
    current_target: &OpenAttachTarget,
) -> io::Result<()> {
    emit_attach_bytes(
        stream,
        format!(
            "[detached (from session {})]\r\n",
            current_target.session_name
        )
        .as_bytes(),
    )
    .await
}

pub(super) async fn emit_exited_message(stream: &LocalStream) -> io::Result<()> {
    emit_attach_bytes(stream, b"[exited]\r\n").await
}

#[cfg(unix)]
pub(super) async fn read_from_pane(
    pane_reader: &AsyncFd<PtyIo>,
    buffer: &mut [u8],
) -> io::Result<usize> {
    loop {
        let mut ready = pane_reader.readable().await?;
        match ready.try_io(|inner| inner.get_ref().read(&mut *buffer)) {
            Ok(Ok(bytes_read)) => return Ok(bytes_read),
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error))
                if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
            {
                return Ok(0);
            }
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => continue,
        }
    }
}

async fn write_all_to_stream(stream: &LocalStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.writable().await?;

        match stream.try_write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write returned 0 while forwarding pane bytes",
                ));
            }
            Ok(bytes_written) => bytes = &bytes[bytes_written..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if is_peer_disconnect(&error) => return Ok(()),
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

pub(super) fn invalid_attach_message(error: rmux_proto::RmuxError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
