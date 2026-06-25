use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const TRACE_ENV: &str = "RMUX_WINDOWS_PERF_TRACE";

pub(super) struct Span {
    event: &'static str,
    started_at: Instant,
}

pub(super) fn span(event: &'static str) -> Span {
    Span {
        event,
        started_at: Instant::now(),
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        write_record(self.event, self.started_at.elapsed().as_micros());
    }
}

fn trace_path() -> Option<&'static OsString> {
    static TRACE_PATH: OnceLock<Option<OsString>> = OnceLock::new();
    TRACE_PATH
        .get_or_init(|| std::env::var_os(TRACE_ENV).filter(|path| !path.is_empty()))
        .as_ref()
}

fn write_record(event: &str, duration_us: u128) {
    let Some(path) = trace_path() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };

    let ts_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or_default();
    let mut line = String::new();
    let _ = write!(
        line,
        "{{\"ts_us\":{},\"pid\":{},\"target\":\"rmux-pty-windows\",\"event\":\"",
        ts_us,
        std::process::id()
    );
    push_json_string_fragment(&mut line, event);
    let _ = writeln!(line, "\",\"duration_us\":{duration_us}}}");
    let _ = file.write_all(line.as_bytes());
}

fn push_json_string_fragment(line: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '"' => line.push_str("\\\""),
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            '\r' => line.push_str("\\r"),
            '\t' => line.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(line, "\\u{:04x}", ch as u32);
            }
            ch => line.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::push_json_string_fragment;

    #[test]
    fn json_string_escape_covers_control_characters() {
        let mut escaped = String::new();
        push_json_string_fragment(&mut escaped, "a\"b\\c\n");

        assert_eq!(escaped, "a\\\"b\\\\c\\n");
    }
}
