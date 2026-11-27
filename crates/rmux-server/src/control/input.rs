use std::collections::VecDeque;
use std::io;

use rmux_proto::format_exit_line;
use tokio::io::AsyncWrite;

use super::output_queue::{flush_bounded, write_all_bounded};

#[cfg(all(test, windows))]
#[path = "input_windows_tests.rs"]
mod windows_tests;

pub(crate) const MAX_CONTROL_LINE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_QUEUED_CONTROL_LINES: usize = 1024;
pub(super) const MAX_QUEUED_CONTROL_BYTES: usize = 4 * 1024 * 1024;

pub(super) async fn append_control_input_with_rejection(
    input_buffer: &mut Vec<u8>,
    queued_lines: &mut VecDeque<String>,
    queued_input_bytes: &mut usize,
    bytes: &[u8],
    writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<()> {
    match append_control_input(input_buffer, queued_lines, queued_input_bytes, bytes) {
        Ok(()) => Ok(()),
        Err(error) => {
            // This transport-level rejection has no accepted command frame.
            // Bound delivery just like ordinary control output so a non-reader
            // cannot retain this connection forever or block another client.
            let rejection = format_exit_line(Some(&error.to_string()));
            write_all_bounded(writer, rejection.as_bytes()).await?;
            flush_bounded(writer).await?;
            Err(error)
        }
    }
}

pub(super) fn extract_complete_control_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();

    while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut line = buffer.drain(..=position).collect::<Vec<_>>();
        if matches!(line.last(), Some(b'\n')) {
            let _ = line.pop();
        }
        if matches!(line.last(), Some(b'\r')) {
            let _ = line.pop();
        }
        lines.push(String::from_utf8_lossy(&line).into_owned());
    }

    lines
}

pub(super) fn append_control_input(
    input_buffer: &mut Vec<u8>,
    queued_lines: &mut VecDeque<String>,
    queued_input_bytes: &mut usize,
    bytes: &[u8],
) -> io::Result<()> {
    input_buffer.extend_from_slice(bytes);
    let lines = extract_complete_control_lines(input_buffer);
    if input_buffer.len() > MAX_CONTROL_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("control input line exceeds {MAX_CONTROL_LINE_BYTES} bytes without a newline"),
        ));
    }
    for line in lines {
        if line.len() > MAX_CONTROL_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("control input line exceeds {MAX_CONTROL_LINE_BYTES} bytes"),
            ));
        }
        let next_line_count = queued_lines.len().saturating_add(1);
        let next_byte_count = queued_input_bytes.saturating_add(line.len());
        if next_line_count > MAX_QUEUED_CONTROL_LINES || next_byte_count > MAX_QUEUED_CONTROL_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "control input queue exceeds {MAX_QUEUED_CONTROL_LINES} lines or {MAX_QUEUED_CONTROL_BYTES} bytes"
                ),
            ));
        }
        *queued_input_bytes = next_byte_count;
        queued_lines.push_back(line);
    }
    Ok(())
}
