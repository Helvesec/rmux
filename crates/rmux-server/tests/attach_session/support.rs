use std::error::Error;
use std::io;
use std::path::Path;
use std::time::Duration;

use rmux_proto::{AttachMessage, Request, Response};
use tokio::io::AsyncReadExt;
use tokio::time::sleep;

pub(super) const STEP_TIMEOUT: Duration = Duration::from_secs(12);

pub(super) async fn read_response_exact(
    stream: &mut tokio::net::UnixStream,
) -> Result<Response, Box<dyn Error>> {
    tokio::time::timeout(STEP_TIMEOUT, crate::common::read_response_exact(stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out reading response frame"))?
}

pub(super) async fn read_attach_message(
    stream: &mut tokio::net::UnixStream,
) -> Result<Option<AttachMessage>, Box<dyn Error>> {
    let mut tag = [0_u8; 1];
    let bytes_read = stream.read(&mut tag).await?;
    if bytes_read == 0 {
        return Ok(None);
    }

    match tag[0] {
        1 => {
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).await?;
            let payload_len = u32::from_le_bytes(length) as usize;
            let mut payload = vec![0_u8; payload_len];
            stream.read_exact(&mut payload).await?;
            Ok(Some(AttachMessage::Data(payload)))
        }
        2 => {
            let mut size = [0_u8; 4];
            stream.read_exact(&mut size).await?;
            Ok(Some(AttachMessage::Resize(rmux_proto::TerminalSize {
                cols: u16::from_le_bytes([size[0], size[1]]),
                rows: u16::from_le_bytes([size[2], size[3]]),
            })))
        }
        13 => {
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).await?;
            let payload_len = u32::from_le_bytes(length) as usize;
            let mut payload = vec![0_u8; payload_len];
            stream.read_exact(&mut payload).await?;
            Ok(Some(AttachMessage::Render(payload)))
        }
        other => Err(rmux_proto::RmuxError::Decode(format!(
            "unknown attach-stream message tag {other}"
        ))
        .into()),
    }
}

pub(super) async fn read_attach_until_contains(
    stream: &mut tokio::net::UnixStream,
    needle: &str,
    timeout_duration: Duration,
) -> Result<String, Box<dyn Error>> {
    let deadline = std::time::Instant::now() + timeout_duration;
    let mut output = String::new();

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let read = tokio::time::timeout(remaining, read_attach_message(stream)).await;
        let Some(message) = read.map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out waiting for attach output containing {needle:?}: {output:?}"),
            )
        })??
        else {
            break;
        };

        if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
            output.push_str(&String::from_utf8_lossy(&bytes));
            if output.contains(needle) {
                return Ok(output);
            }
        }
    }

    Err(io::Error::other(format!(
        "timed out waiting for attach output containing {needle:?}: {output:?}"
    ))
    .into())
}

pub(super) async fn read_attach_until_eof(
    stream: &mut tokio::net::UnixStream,
    timeout_duration: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + timeout_duration;
    let mut buffer = [0_u8; 256];

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let bytes_read = tokio::time::timeout(remaining, stream.read(&mut buffer)).await??;
        if bytes_read == 0 {
            return Ok(());
        }
    }
}

pub(super) async fn retry_request_until(
    socket_path: &Path,
    request: &Request,
    expected: &Response,
) -> Result<Response, Box<dyn Error>> {
    for _ in 0..20 {
        let response = crate::common::send_request(socket_path, request).await?;
        if &response == expected {
            return Ok(response);
        }
        sleep(Duration::from_millis(10)).await;
    }

    crate::common::send_request(socket_path, request).await
}
