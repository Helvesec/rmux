//! Minimal JSONL tracing for terminal automation workflows.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::{Pane, Result, Rmux, RmuxError};

const DEFAULT_MAX_TRACE_EVENTS: usize = 100_000;

/// Builder for a minimal trace session.
#[derive(Debug)]
pub struct RmuxTraceBuilder<'a> {
    rmux: &'a Rmux,
    max_events: usize,
}

impl<'a> RmuxTraceBuilder<'a> {
    pub(crate) const fn new(rmux: &'a Rmux) -> Self {
        Self {
            rmux,
            max_events: DEFAULT_MAX_TRACE_EVENTS,
        }
    }

    /// Caps the number of events retained in memory before [`Self::start`].
    ///
    /// When the cap is reached, the oldest event is dropped. The trace API is
    /// intentionally minimal and in-memory; use a small cap for long-running
    /// processes that only need recent automation context.
    pub const fn max_events(mut self, max_events: usize) -> Self {
        self.max_events = max_events;
        self
    }

    /// Starts an in-memory trace buffer.
    ///
    /// The trace is written when [`TraceSession::stop`] is called. Events are
    /// buffered in memory up to the configured cap. The SDK does not install
    /// global hooks; callers record events explicitly.
    pub async fn start(self) -> Result<TraceSession> {
        let session = TraceSession {
            endpoint: format!("{:?}", self.rmux.resolved_endpoint()?),
            max_events: self.max_events,
            events: Arc::new(Mutex::new(VecDeque::new())),
        };
        session.record("trace.start", None::<TracePayload>)?;
        Ok(session)
    }
}

/// Active minimal trace session.
#[derive(Debug, Clone)]
pub struct TraceSession {
    endpoint: String,
    max_events: usize,
    events: Arc<Mutex<VecDeque<TraceEvent>>>,
}

impl TraceSession {
    /// Records a free-form action event.
    pub fn record_action(&self, action: impl Into<String>) -> Result<()> {
        self.record(
            "action",
            Some(TracePayload {
                action: Some(action.into()),
                ..TracePayload::default()
            }),
        )
    }

    /// Records input sent to a pane.
    pub fn record_input(&self, pane: &Pane, input: impl Into<String>) -> Result<()> {
        self.record(
            "input",
            Some(TracePayload {
                pane: Some(format!("{}", pane.target().to_proto())),
                input: Some(input.into()),
                ..TracePayload::default()
            }),
        )
    }

    /// Captures and records the pane's current visible snapshot text.
    pub async fn record_snapshot(&self, pane: &Pane) -> Result<()> {
        let snapshot = pane.snapshot().await?;
        self.record(
            "snapshot",
            Some(TracePayload {
                pane: Some(format!("{}", pane.target().to_proto())),
                revision: Some(snapshot.revision),
                snapshot: Some(snapshot.visible_text()),
                ..TracePayload::default()
            }),
        )
    }

    /// Stops tracing and writes `trace.jsonl` into `directory`.
    pub async fn stop(self, directory: impl AsRef<Path>) -> Result<PathBuf> {
        self.record("trace.stop", None::<TracePayload>)?;
        let directory = directory.as_ref();
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(trace_io_error)?;
        let path = directory.join("trace.jsonl");
        let events = {
            let events = self.events.lock().map_err(lock_error)?;
            events.iter().cloned().collect::<Vec<_>>()
        };
        let lines = events
            .iter()
            .map(serde_json::to_string)
            .collect::<core::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                RmuxError::from_proto(rmux_proto::RmuxError::Server(format!(
                    "failed to encode rmux trace event: {error}"
                )))
            })?
            .join("\n");
        tokio::fs::write(&path, format!("{lines}\n"))
            .await
            .map_err(trace_io_error)?;
        Ok(path)
    }

    fn record(&self, kind: &'static str, payload: Option<TracePayload>) -> Result<()> {
        let mut events = self.events.lock().map_err(lock_error)?;
        if self.max_events == 0 {
            return Ok(());
        }
        if events.len() == self.max_events {
            events.pop_front();
        }
        events.push_back(TraceEvent {
            timestamp_ms: timestamp_ms(),
            endpoint: self.endpoint.clone(),
            kind,
            payload,
        });
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
struct TraceEvent {
    timestamp_ms: u128,
    endpoint: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<TracePayload>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct TracePayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<String>,
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn trace_io_error(error: std::io::Error) -> RmuxError {
    RmuxError::transport("write rmux trace", error)
}

fn lock_error<T>(error: std::sync::PoisonError<T>) -> RmuxError {
    RmuxError::from_proto(rmux_proto::RmuxError::Server(format!(
        "rmux trace lock poisoned: {error}"
    )))
}
