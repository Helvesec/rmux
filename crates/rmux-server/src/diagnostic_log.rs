use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const EXIT_LOG_ENV: &str = "RMUX_ATTACH_EXIT_LOG";

pub(crate) fn record_shutdown_request(reason: &str) {
    record_line(&format!(
        "time_ms={} process_pid={} event=shutdown-request reason={reason}",
        timestamp_ms(),
        std::process::id()
    ));
}

pub(crate) fn record_shutdown_queued(reason: &str) {
    record_line(&format!(
        "time_ms={} process_pid={} event=shutdown-queued reason={reason}",
        timestamp_ms(),
        std::process::id()
    ));
}

fn record_line(line: &str) {
    let Some(path) = std::env::var_os(EXIT_LOG_ENV) else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
