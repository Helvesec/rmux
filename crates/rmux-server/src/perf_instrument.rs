#[cfg(feature = "perf-instrument")]
mod enabled {
    use std::fmt::Write as _;
    use std::fs::{File, OpenOptions};
    use std::io::Write as _;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    const TRACE_ENV: &str = "RMUX_PERF_TRACE";

    static SINK: OnceLock<TraceSink> = OnceLock::new();

    struct TraceSink {
        file: Mutex<File>,
    }

    enum FieldValue {
        U64(u64),
        Usize(usize),
        Bool(bool),
        Str(String),
    }

    struct Field {
        key: &'static str,
        value: FieldValue,
    }

    pub(crate) struct Span {
        event: &'static str,
        started_at: Instant,
        fields: Vec<Field>,
    }

    pub(crate) struct Event {
        event: &'static str,
        fields: Vec<Field>,
    }

    pub(crate) fn span(event: &'static str) -> Span {
        Span {
            event,
            started_at: Instant::now(),
            fields: Vec::new(),
        }
    }

    pub(crate) fn event(event: &'static str) -> Event {
        Event {
            event,
            fields: Vec::new(),
        }
    }

    impl Span {
        pub(crate) fn with_u64(mut self, key: &'static str, value: u64) -> Self {
            self.fields.push(Field {
                key,
                value: FieldValue::U64(value),
            });
            self
        }

        pub(crate) fn with_usize(mut self, key: &'static str, value: usize) -> Self {
            self.fields.push(Field {
                key,
                value: FieldValue::Usize(value),
            });
            self
        }

        pub(crate) fn with_str(mut self, key: &'static str, value: impl Into<String>) -> Self {
            self.fields.push(Field {
                key,
                value: FieldValue::Str(value.into()),
            });
            self
        }
    }

    impl Drop for Span {
        fn drop(&mut self) {
            write_record(
                "span",
                self.event,
                Some(self.started_at.elapsed().as_micros()),
                &self.fields,
            );
        }
    }

    impl Event {
        pub(crate) fn with_usize(mut self, key: &'static str, value: usize) -> Self {
            self.fields.push(Field {
                key,
                value: FieldValue::Usize(value),
            });
            self
        }

        pub(crate) fn with_bool(mut self, key: &'static str, value: bool) -> Self {
            self.fields.push(Field {
                key,
                value: FieldValue::Bool(value),
            });
            self
        }

        pub(crate) fn with_str(mut self, key: &'static str, value: impl Into<String>) -> Self {
            self.fields.push(Field {
                key,
                value: FieldValue::Str(value.into()),
            });
            self
        }

        pub(crate) fn emit(self) {
            write_record("event", self.event, None, &self.fields);
        }
    }

    fn sink() -> Option<&'static TraceSink> {
        if let Some(sink) = SINK.get() {
            return Some(sink);
        }
        let path = std::env::var_os(TRACE_ENV)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        let _ = SINK.set(TraceSink {
            file: Mutex::new(file),
        });
        SINK.get()
    }

    fn write_record(kind: &str, event: &str, duration_us: Option<u128>, fields: &[Field]) {
        let Some(sink) = sink() else {
            return;
        };
        let mut line = String::new();
        let ts_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros())
            .unwrap_or_default();
        let thread = format!("{:?}", std::thread::current().id());
        let _ = write!(
            line,
            "{{\"ts_us\":{},\"pid\":{},\"thread\":\"",
            ts_us,
            std::process::id()
        );
        push_json_string_fragment(&mut line, &thread);
        let _ = write!(line, "\",\"kind\":\"{kind}\",\"event\":\"");
        push_json_string_fragment(&mut line, event);
        line.push('"');
        if let Some(duration_us) = duration_us {
            let _ = write!(line, ",\"duration_us\":{duration_us}");
        }
        for field in fields {
            line.push(',');
            line.push('"');
            push_json_string_fragment(&mut line, field.key);
            line.push_str("\":");
            push_field_value(&mut line, &field.value);
        }
        line.push_str("}\n");
        if let Ok(mut file) = sink.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn push_field_value(line: &mut String, value: &FieldValue) {
        match value {
            FieldValue::U64(value) => {
                let _ = write!(line, "{value}");
            }
            FieldValue::Usize(value) => {
                let _ = write!(line, "{value}");
            }
            FieldValue::Bool(value) => {
                let _ = write!(line, "{value}");
            }
            FieldValue::Str(value) => {
                line.push('"');
                push_json_string_fragment(line, value);
                line.push('"');
            }
        }
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
        use std::fs;

        use super::{event, push_json_string_fragment, span, TRACE_ENV};

        #[test]
        fn json_string_escape_covers_control_characters() {
            let mut escaped = String::new();
            push_json_string_fragment(&mut escaped, "a\"b\\c\n");
            assert_eq!(escaped, "a\\\"b\\\\c\\n");
        }

        #[test]
        fn trace_file_receives_span_and_event_records() {
            let path = std::env::temp_dir().join(format!(
                "rmux-perf-trace-{}-{}.jsonl",
                std::process::id(),
                line!()
            ));
            let _ = fs::remove_file(&path);
            std::env::set_var(TRACE_ENV, &path);

            {
                let _span = span("snapshot")
                    .with_u64("pane_id", 7)
                    .with_usize("cells", 80)
                    .with_str("site", "unit-test");
            }
            event("queue_backpressure")
                .with_str("result", "backpressure")
                .with_bool("coalesced", false)
                .emit();

            let trace = fs::read_to_string(&path).expect("trace file must be written");
            assert!(trace.contains("\"kind\":\"span\""));
            assert!(trace.contains("\"event\":\"snapshot\""));
            assert!(trace.contains("\"duration_us\":"));
            assert!(trace.contains("\"kind\":\"event\""));
            assert!(trace.contains("\"event\":\"queue_backpressure\""));
            assert!(trace.contains("\"coalesced\":false"));

            std::env::remove_var(TRACE_ENV);
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(not(feature = "perf-instrument"))]
#[allow(dead_code)]
mod disabled {
    pub(crate) struct Span;
    pub(crate) struct Event;

    pub(crate) fn span(_event: &'static str) -> Span {
        Span
    }

    pub(crate) fn event(_event: &'static str) -> Event {
        Event
    }

    impl Span {
        pub(crate) fn with_u64(self, _key: &'static str, _value: u64) -> Self {
            self
        }

        pub(crate) fn with_usize(self, _key: &'static str, _value: usize) -> Self {
            self
        }

        pub(crate) fn with_str(self, _key: &'static str, _value: impl Into<String>) -> Self {
            self
        }
    }

    impl Event {
        pub(crate) fn with_usize(self, _key: &'static str, _value: usize) -> Self {
            self
        }

        pub(crate) fn with_bool(self, _key: &'static str, _value: bool) -> Self {
            self
        }

        pub(crate) fn with_str(self, _key: &'static str, _value: impl Into<String>) -> Self {
            self
        }

        pub(crate) fn emit(self) {}
    }
}

#[cfg(not(feature = "perf-instrument"))]
#[allow(unused_imports)]
pub(crate) use disabled::{event, span};
#[cfg(feature = "perf-instrument")]
pub(crate) use enabled::{event, span};
