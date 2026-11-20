use super::*;

pub(super) fn write_timeout_result(
    json_output: bool,
    condition: &'static str,
    target: &str,
    timeout: Duration,
) -> Result<i32, ExitFailure> {
    if json_output {
        return write_json(&json!({
            "schema_version": 1,
            "ok": false,
            "error": "timeout",
            "condition": condition,
            "target": target,
            "timeout_ms": duration_millis(timeout),
        }))
        .map(|_| 1);
    }
    Err(ExitFailure::new(
        1,
        format!("wait-pane timed out waiting for {condition}"),
    ))
}

pub(super) fn timeout_error(condition: &'static str, started_at: Instant) -> ExitFailure {
    ExitFailure::new(
        1,
        format!(
            "send-keys timed out waiting for {condition} after {} ms",
            elapsed_millis(started_at)
        ),
    )
}

pub(super) fn next_text_lag_error(missed_events: u64) -> ExitFailure {
    ExitFailure::new(
        1,
        format!(
            "wait-pane --next-text lost pane output due to lag before matching; missed {missed_events} events"
        ),
    )
}

pub(super) fn write_lag_result(
    json_output: bool,
    condition: &'static str,
    target: &str,
    missed_events: u64,
) -> Result<i32, ExitFailure> {
    if json_output {
        return write_json(&lag_json_value(condition, target, missed_events)).map(|_| 1);
    }
    Err(next_text_lag_error(missed_events))
}

pub(super) fn lag_json_value(condition: &'static str, target: &str, missed_events: u64) -> Value {
    json!({
        "schema_version": 1,
        "ok": false,
        "error": "pane-output-lag",
        "condition": condition,
        "target": target,
        "missed_events": missed_events,
    })
}
