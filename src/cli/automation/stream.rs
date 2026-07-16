use std::path::Path;

use rmux_proto::{PaneOutputSubscriptionId, PaneOutputSubscriptionStart, Response};
use serde_json::json;

use crate::cli_args::{CollectPaneOutputArgs, StreamPaneArgs};
use crate::cli_response::tmux_cli_error_message;

use super::super::ExitFailure;
use super::common::{
    check_disabled, connect_cli, pane_process_state, pane_snapshot, resolve_pane_ref,
    sleep_poll_interval, stdout_closed, visible_lines, visible_text, write_json, write_stderr_line,
    write_stdout_bytes, write_stdout_bytes_or_broken_pipe, PaneProcessState, StdoutWrite,
    SCHEMA_VERSION,
};

const CURSOR_BATCH_EVENTS: u16 = 128;
const LINE_BUFFER_MAX: usize = 1_048_576;

pub(crate) fn run_stream_pane(
    args: StreamPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    check_disabled("RMUX_DISABLE_STREAM_PANE", "stream-pane")?;
    let mut connection = connect_cli(socket_path)?;
    let target = resolve_pane_ref(&mut connection, args.target.as_ref(), "stream-pane")?;
    let subscription_id = subscribe(
        &mut connection,
        target.clone(),
        PaneOutputSubscriptionStart::Oldest,
    )?;
    let line_mode = args.lines && !args.raw;
    let mut line_buffer = Vec::new();
    let mut line_buffer_force_flushed = false;
    let mut wrote_stdout = false;
    loop {
        if stdout_closed() {
            let _ = connection.unsubscribe_pane_output(subscription_id);
            return Ok(0);
        }
        let batch = poll_output(&mut connection, subscription_id, "stream-pane")?;
        if batch.lag.is_some() {
            line_buffer.clear();
            line_buffer_force_flushed = false;
            if !wrote_stdout {
                match write_lag_snapshot_seed(&mut connection, target.clone(), line_mode)? {
                    LagSnapshotSeed::Written => wrote_stdout = true,
                    LagSnapshotSeed::BrokenPipe => {
                        let _ = connection.unsubscribe_pane_output(subscription_id);
                        return Ok(0);
                    }
                    LagSnapshotSeed::Empty => {}
                }
            }
        }
        for bytes in batch.chunks {
            if line_mode {
                if write_lines(&mut line_buffer, &mut line_buffer_force_flushed, &bytes)? {
                    let _ = connection.unsubscribe_pane_output(subscription_id);
                    return Ok(0);
                }
                wrote_stdout |= bytes.contains(&b'\n');
            } else {
                if matches!(
                    write_stdout_bytes_or_broken_pipe(&bytes)?,
                    StdoutWrite::BrokenPipe
                ) {
                    let _ = connection.unsubscribe_pane_output(subscription_id);
                    return Ok(0);
                }
                wrote_stdout = true;
            }
        }
        if batch.saw_eof {
            if line_mode && flush_line_buffer(&mut line_buffer)? {
                let _ = connection.unsubscribe_pane_output(subscription_id);
                return Ok(0);
            }
            let _ = connection.unsubscribe_pane_output(subscription_id);
            return Ok(0);
        }
        sleep_poll_interval();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LagSnapshotSeed {
    Empty,
    Written,
    BrokenPipe,
}

fn write_lag_snapshot_seed(
    connection: &mut rmux_client::Connection,
    target: rmux_proto::PaneTargetRef,
    line_mode: bool,
) -> Result<LagSnapshotSeed, ExitFailure> {
    let snapshot = pane_snapshot(connection, target)?;
    if line_mode {
        let mut wrote = false;
        for line in visible_lines(&snapshot) {
            if line.is_empty() {
                continue;
            }
            if matches!(write_line(line.into_bytes())?, StdoutWrite::BrokenPipe) {
                return Ok(LagSnapshotSeed::BrokenPipe);
            }
            wrote = true;
        }
        return Ok(if wrote {
            LagSnapshotSeed::Written
        } else {
            LagSnapshotSeed::Empty
        });
    }

    let seed = visible_text(&snapshot);
    if seed.is_empty() {
        return Ok(LagSnapshotSeed::Empty);
    }
    if matches!(
        write_stdout_bytes_or_broken_pipe(seed.as_bytes())?,
        StdoutWrite::BrokenPipe
    ) {
        return Ok(LagSnapshotSeed::BrokenPipe);
    }
    Ok(LagSnapshotSeed::Written)
}

pub(crate) fn run_collect_pane_output(
    args: CollectPaneOutputArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    check_disabled("RMUX_DISABLE_STREAM_PANE", "collect-pane-output")?;
    let mut connection = connect_cli(socket_path)?;
    let target_ref =
        resolve_pane_ref(&mut connection, args.target.as_ref(), "collect-pane-output")?;
    let subscription_id = subscribe(
        &mut connection,
        target_ref.clone(),
        PaneOutputSubscriptionStart::Oldest,
    )?;
    let mut output = Vec::new();
    let mut total_bytes: usize = 0;
    let mut truncated = false;
    let mut pane_exit = None;
    let mut saw_eof = false;
    let mut missed_events = 0_u64;
    loop {
        let batch = poll_output(&mut connection, subscription_id, "collect-pane-output")?;
        saw_eof |= batch.saw_eof;
        if let Some(lag) = batch.lag {
            missed_events = missed_events.saturating_add(lag.missed_events);
        }
        for bytes in batch.chunks {
            total_bytes = total_bytes.saturating_add(bytes.len());
            if output.len() < args.max_bytes {
                let remaining = args.max_bytes - output.len();
                let keep = bytes.len().min(remaining);
                output.extend_from_slice(&bytes[..keep]);
                truncated |= keep < bytes.len();
            } else {
                truncated = true;
            }
        }
        if pane_exit.is_none() {
            if let PaneProcessState::Exited(value) =
                pane_process_state(&mut connection, &target_ref)?
            {
                pane_exit = Some(value);
            }
        }
        if saw_eof && pane_exit.is_some() {
            break;
        }
        sleep_poll_interval();
    }
    let _ = connection.unsubscribe_pane_output(subscription_id);
    let pane_exit = pane_exit.expect("pane exit is observed before collect-pane-output breaks");
    if missed_events > 0 {
        if args.json {
            return write_json(&json!({
                "schema_version": SCHEMA_VERSION,
                "ok": false,
                "error": "pane-output-lag",
                "bytes": total_bytes,
                "stored_bytes": output.len(),
                "truncated": truncated,
                "missed_events": missed_events,
                "pane_exit": pane_exit.json_value(),
            }))
            .map(|_| 1);
        }
        return Err(ExitFailure::new(
            1,
            format!(
                "collect-pane-output lost pane output due to lag; missed {missed_events} events"
            ),
        ));
    }
    if args.json {
        return write_json(&json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "bytes": total_bytes,
            "stored_bytes": output.len(),
            "truncated": truncated,
            "output_utf8_lossy": String::from_utf8_lossy(&output),
            "pane_exit": pane_exit.json_value(),
        }));
    }
    write_stdout_bytes(&output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutputLag {
    pub(super) missed_events: u64,
}

pub(super) struct OutputBatch {
    pub(super) chunks: Vec<Vec<u8>>,
    pub(super) saw_eof: bool,
    pub(super) lag: Option<OutputLag>,
}

pub(super) fn subscribe(
    connection: &mut rmux_client::Connection,
    target: rmux_proto::PaneTargetRef,
    start: PaneOutputSubscriptionStart,
) -> Result<PaneOutputSubscriptionId, ExitFailure> {
    match connection
        .subscribe_pane_output_ref(target, start)
        .map_err(ExitFailure::from_client)?
    {
        Response::SubscribePaneOutput(response) => Ok(response.subscription_id),
        Response::Error(error) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message("stream-pane", &error.error),
        )),
        other => Err(ExitFailure::new(
            1,
            format!(
                "protocol error: unexpected '{}' response for stream-pane",
                other.command_name()
            ),
        )),
    }
}

pub(super) fn poll_output(
    connection: &mut rmux_client::Connection,
    subscription_id: PaneOutputSubscriptionId,
    command_name: &'static str,
) -> Result<OutputBatch, ExitFailure> {
    poll_output_inner(connection, subscription_id, command_name, true)
}

pub(super) fn poll_output_silent_lag(
    connection: &mut rmux_client::Connection,
    subscription_id: PaneOutputSubscriptionId,
    command_name: &'static str,
) -> Result<OutputBatch, ExitFailure> {
    poll_output_inner(connection, subscription_id, command_name, false)
}

fn poll_output_inner(
    connection: &mut rmux_client::Connection,
    subscription_id: PaneOutputSubscriptionId,
    command_name: &'static str,
    report_lag: bool,
) -> Result<OutputBatch, ExitFailure> {
    match connection
        .pane_output_cursor(subscription_id, Some(CURSOR_BATCH_EVENTS))
        .map_err(ExitFailure::from_client)?
    {
        Response::PaneOutputCursor(response) => {
            let mut saw_eof = false;
            let chunks = response
                .events
                .into_iter()
                .filter_map(|event| {
                    if event.bytes.is_empty() {
                        saw_eof = true;
                        None
                    } else {
                        Some(event.bytes)
                    }
                })
                .collect();
            Ok(OutputBatch {
                chunks,
                saw_eof,
                lag: None,
            })
        }
        Response::PaneOutputLag(response) => {
            if report_lag {
                write_stderr_line(&format!(
                    "{command_name}: pane output lagged; missed {} events",
                    response.lag.missed_events
                ));
            }
            Ok(OutputBatch {
                chunks: Vec::new(),
                saw_eof: false,
                lag: Some(OutputLag {
                    missed_events: response.lag.missed_events,
                }),
            })
        }
        Response::Error(error) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message(command_name, &error.error),
        )),
        other => Err(ExitFailure::new(
            1,
            format!(
                "protocol error: unexpected '{}' response for {command_name}",
                other.command_name()
            ),
        )),
    }
}

fn write_lines(
    buffer: &mut Vec<u8>,
    force_flushed: &mut bool,
    bytes: &[u8],
) -> Result<bool, ExitFailure> {
    let mut broken_pipe = false;
    split_lines_bounded(buffer, force_flushed, bytes, |line| {
        if matches!(write_line(line)?, StdoutWrite::BrokenPipe) {
            broken_pipe = true;
        }
        Ok(())
    })?;
    Ok(broken_pipe)
}

fn flush_line_buffer(buffer: &mut Vec<u8>) -> Result<bool, ExitFailure> {
    let mut broken_pipe = false;
    flush_line_buffer_into(buffer, |line| {
        if matches!(write_line(line)?, StdoutWrite::BrokenPipe) {
            broken_pipe = true;
        }
        Ok(())
    })?;
    Ok(broken_pipe)
}

fn flush_line_buffer_into<F>(buffer: &mut Vec<u8>, mut emit: F) -> Result<(), ExitFailure>
where
    F: FnMut(Vec<u8>) -> Result<(), ExitFailure>,
{
    if buffer.is_empty() {
        return Ok(());
    }
    emit(std::mem::take(buffer))
}

fn write_line(mut line: Vec<u8>) -> Result<StdoutWrite, ExitFailure> {
    if line.ends_with(b"\r") {
        line.pop();
    }
    let text = String::from_utf8_lossy(&line);
    if matches!(
        write_stdout_bytes_or_broken_pipe(text.as_bytes())?,
        StdoutWrite::BrokenPipe
    ) {
        return Ok(StdoutWrite::BrokenPipe);
    }
    write_stdout_bytes_or_broken_pipe(b"\n")
}

fn split_lines_bounded<F>(
    buffer: &mut Vec<u8>,
    force_flushed: &mut bool,
    bytes: &[u8],
    mut emit: F,
) -> Result<(), ExitFailure>
where
    F: FnMut(Vec<u8>) -> Result<(), ExitFailure>,
{
    for byte in bytes {
        if *byte == b'\n' {
            if buffer.is_empty() && *force_flushed {
                *force_flushed = false;
                continue;
            }
            emit(std::mem::take(buffer))?;
            *force_flushed = false;
        } else {
            buffer.push(*byte);
            *force_flushed = false;
            if buffer.len() >= LINE_BUFFER_MAX {
                emit(std::mem::take(buffer))?;
                *force_flushed = true;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{flush_line_buffer_into, split_lines_bounded, LINE_BUFFER_MAX};

    #[test]
    fn line_stream_buffer_is_force_flushed_at_fixed_limit() {
        let mut buffer = Vec::new();
        let mut force_flushed = false;
        let bytes = vec![b'a'; LINE_BUFFER_MAX + 5];
        let mut lines = Vec::new();

        split_lines_bounded(&mut buffer, &mut force_flushed, &bytes, |line| {
            lines.push(line);
            Ok(())
        })
        .unwrap();

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), LINE_BUFFER_MAX);
        assert_eq!(buffer.len(), 5);
    }

    #[test]
    fn line_stream_skips_newline_after_force_flush() {
        let mut buffer = Vec::new();
        let mut force_flushed = false;
        let bytes = vec![b'a'; LINE_BUFFER_MAX];
        let mut lines = Vec::new();

        split_lines_bounded(&mut buffer, &mut force_flushed, &bytes, |line| {
            lines.push(line);
            Ok(())
        })
        .unwrap();
        split_lines_bounded(&mut buffer, &mut force_flushed, b"\n", |line| {
            lines.push(line);
            Ok(())
        })
        .unwrap();

        assert_eq!(lines.len(), 1);
        assert!(buffer.is_empty());
        assert!(!force_flushed);
    }

    #[test]
    fn line_stream_flushes_final_partial_line_on_eof() {
        let mut buffer = b"FINAL".to_vec();
        let mut lines = Vec::new();

        flush_line_buffer_into(&mut buffer, |line| {
            lines.push(line);
            Ok(())
        })
        .unwrap();

        assert_eq!(lines, vec![b"FINAL".to_vec()]);
        assert!(buffer.is_empty());
    }
}
