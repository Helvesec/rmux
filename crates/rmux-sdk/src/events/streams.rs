//! SDK-owned facade over the v1 pane-output subscription protocol.
//!
//! The two opaque streams in this module — [`PaneOutputStream`] for raw
//! bytes plus sequence/lag notices, and [`PaneLineStream`] for rendered
//! UTF-8 lines — are constructed through fallible [`crate::Pane`] methods and
//! drive the daemon's `SubscribePaneOutput`, `PaneOutputCursor`, and
//! `UnsubscribePaneOutput` endpoints internally. They never expose
//! [`rmux_proto::PaneOutputSubscriptionId`] to SDK callers.
//!
//! ## Raw bytes vs rendered lines
//!
//! [`PaneOutputStream`] emits [`PaneOutputChunk`] items. Bytes
//! ([`PaneOutputChunk::Bytes`]) preserve every payload byte the daemon
//! delivered, including NUL and bytes that are not valid UTF-8, and pair
//! them with the monotonic per-pane [`PaneOutputChunk::Bytes::sequence`]
//! the daemon assigned. Lag notices ([`PaneOutputChunk::Lag`]) surface
//! the daemon-side gap between the cursor's expected sequence and the
//! oldest retained sequence verbatim, including the bounded recent live
//! bytes the daemon retained at gap detection time. The raw byte stream
//! never converts payloads through `String::from_utf8_lossy` and never
//! alters the byte sequence the daemon delivered.
//!
//! [`PaneLineStream`] is a strict superset built on top of the raw stream
//! that adds two well-isolated transformations:
//!
//! * **Lossy UTF-8 rendering.** Each completed line's bytes are decoded
//!   through `String::from_utf8_lossy`, which replaces every byte
//!   sequence that is not valid UTF-8 with the Unicode replacement
//!   character `U+FFFD`. The lossy conversion is applied only when the
//!   line is yielded — not on the underlying byte stream — so a caller
//!   that wants byte-faithful output should use [`PaneOutputStream`]
//!   instead. Embedded NUL bytes survive into the rendered string as
//!   `\0`, only invalid UTF-8 byte sequences are replaced.
//! * **Partial-line buffering.** The line stream splits on the LF byte
//!   `b'\n'` only. Carriage returns and any other bytes are preserved
//!   inside the line. Bytes that are not yet terminated by an LF stay in
//!   an internal buffer and are not yielded unless the buffer reaches its
//!   hard safety limit. A trailing partial line that the daemon never
//!   terminates with LF is dropped when the stream ends or lag fires,
//!   because the next sequence's bytes may not begin at a line boundary.
//!
//! On a [`PaneOutputChunk::Lag`] the line stream drops the partial-line
//! buffer (the next sequence may be discontinuous with the buffered
//! bytes), forwards the lag notice as [`PaneLineItem::Lag`], and resumes
//! line splitting from a clean state on subsequent bytes. Callers that
//! want to recover the dropped partial bytes can read
//! [`PaneLagNotice::recent`].
//!
//! ## Drop / unsubscribe contract
//!
//! Each stream owns one per-connection subscription on the daemon, and
//! every drop emits at most one best-effort
//! [`UnsubscribePaneOutput`](rmux_proto::UnsubscribePaneOutputRequest)
//! request through the same transport actor. The unsubscribe is fire and
//! forget — its response is discarded, late or duplicate
//! `unsubscribe-pane-output` errors do not propagate, and a closed
//! transport silently no-ops. The daemon's unsubscribe handler only
//! removes the subscription record; it does not close the pane, the
//! window, the session, the underlying child process, or the daemon
//! itself, so dropping an unfinished stream is always safe.
//!
//! Wrapping the line stream around the byte stream means the inner byte
//! stream still owns its own transport drop guard and emits its own
//! unsubscribe — there is exactly one unsubscribe per subscription
//! regardless of which wrapper is dropped.

use std::collections::VecDeque;
use std::time::Duration;

use rmux_proto::{
    PaneOutputCursorRequest, PaneOutputEvent, PaneOutputLagNotice as ProtoLagNotice,
    PaneOutputSubscriptionId, PaneOutputSubscriptionStart, PaneRecentOutput as ProtoRecentOutput,
    PaneTargetRef, Request, Response, SubscribePaneOutputRefRequest, SubscribePaneOutputRequest,
    UnsubscribePaneOutputRequest, CAPABILITY_SDK_PANE_BY_ID,
};

use crate::handles::session::unexpected_response;
use crate::transport::{DropGuard, TransportClient};
use crate::{Result, RmuxError};

const PANE_OUTPUT_BATCH_SIZE: u16 = 256;
const POLL_INITIAL_DELAY: Duration = Duration::from_millis(2);
const POLL_MAX_DELAY: Duration = Duration::from_millis(50);
const LINE_BUFFER_MAX: usize = 1_048_576;

/// Where a pane-output stream should anchor its cursor at subscription time.
///
/// Mirrors the daemon's own
/// [`rmux_proto::PaneOutputSubscriptionStart`]
/// vocabulary as a SDK-owned enum so callers do not depend on
/// `rmux-proto` directly.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneOutputStart {
    /// Start after the newest output currently retained by the pane. The
    /// stream will only deliver bytes the daemon appends after this call.
    #[default]
    Now,
    /// Start at the oldest retained output event, replaying the daemon's
    /// retained backlog before delivering newly produced bytes.
    Oldest,
}

impl PaneOutputStart {
    fn into_proto(self) -> PaneOutputSubscriptionStart {
        match self {
            Self::Now => PaneOutputSubscriptionStart::Now,
            Self::Oldest => PaneOutputSubscriptionStart::Oldest,
        }
    }
}

/// Recent retained pane bytes attached to a [`PaneLagNotice`].
///
/// The byte payload is never converted through `String::from_utf8_lossy`;
/// it is the exact byte run the daemon retained at gap-detection time,
/// bounded by the daemon's `MAX_LAG_RECENT_BYTES` window.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct PaneRecentOutput {
    /// Retained recent raw pane output bytes.
    pub bytes: Vec<u8>,
    /// Oldest output sequence contributing retained bytes.
    pub oldest_sequence: Option<u64>,
    /// Newest output sequence contributing retained bytes.
    pub newest_sequence: Option<u64>,
}

impl PaneRecentOutput {
    fn from_proto(value: ProtoRecentOutput) -> Self {
        Self {
            bytes: value.bytes,
            oldest_sequence: value.oldest_sequence,
            newest_sequence: value.newest_sequence,
        }
    }
}

/// Detailed gap report carried by [`PaneOutputChunk::Lag`].
///
/// Sequence numbers are exact mirrors of the daemon's own per-pane output
/// counter. `expected_sequence` is the next sequence the cursor was
/// waiting for before lag was detected; `resume_sequence` is the oldest
/// retained sequence the daemon will start delivering from again.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct PaneLagNotice {
    /// Sequence the subscriber expected before lag was detected.
    pub expected_sequence: u64,
    /// Oldest retained sequence where the subscriber will resume.
    pub resume_sequence: u64,
    /// Number of output events skipped by this lag notice.
    pub missed_events: u64,
    /// Newest output sequence appended when lag was detected.
    pub newest_sequence: u64,
    /// Bounded recent live output the daemon retained at lag time.
    pub recent: PaneRecentOutput,
}

impl PaneLagNotice {
    fn from_proto(value: ProtoLagNotice) -> Self {
        Self {
            expected_sequence: value.expected_sequence,
            resume_sequence: value.resume_sequence,
            missed_events: value.missed_events,
            newest_sequence: value.newest_sequence,
            recent: PaneRecentOutput::from_proto(value.recent),
        }
    }
}

/// One item delivered by [`PaneOutputStream`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneOutputChunk {
    /// Raw decoded pane bytes paired with the daemon-assigned monotonic
    /// per-pane sequence.
    Bytes {
        /// Per-pane monotonic output sequence.
        sequence: u64,
        /// Arbitrary raw pane bytes — may include NUL or non-UTF-8 byte
        /// sequences.
        bytes: Vec<u8>,
    },
    /// A daemon-side gap report. Subsequent [`Self::Bytes`] chunks resume
    /// at [`PaneLagNotice::resume_sequence`].
    Lag(PaneLagNotice),
}

/// One item delivered by [`PaneLineStream`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneLineItem {
    /// Decoded line text, with `String::from_utf8_lossy` already applied.
    /// The trailing `\n` and any other line-terminator bytes have been
    /// stripped from `Line.text`.
    Line {
        /// Rendered line text.
        text: String,
    },
    /// A daemon-side gap report propagated unchanged from the underlying
    /// raw byte stream. The line stream drops its partial-line buffer
    /// when this fires; subsequent line splitting starts from a clean
    /// state.
    Lag(PaneLagNotice),
}

/// Opaque live stream of pane output bytes plus sequence/lag notices.
///
/// Construction goes through [`Pane::output_stream`](crate::Pane::output_stream).
/// Use [`PaneOutputStream::next`] to drive the cursor; the per-call
/// polling cadence and any backoff is internal and unspecified. The
/// daemon's [`PaneOutputSubscriptionId`] is *not* observable through this
/// type.
pub struct PaneOutputStream {
    inner: PaneSubscription,
    pending: VecDeque<PaneOutputChunk>,
    poll_delay: Duration,
}

/// Opaque live stream of rendered pane output lines.
///
/// Construction goes through [`Pane::line_stream`](crate::Pane::line_stream).
/// See the module docs for the lossy UTF-8 and partial-line buffering
/// rules.
pub struct PaneLineStream {
    inner: PaneOutputStream,
    line_buffer: Vec<u8>,
    line_buffer_force_flushed: bool,
    pending: VecDeque<PaneLineItem>,
}

struct PaneSubscription {
    transport: TransportClient,
    subscription_id: PaneOutputSubscriptionId,
    // The drop guard is held only for its destructor side effect: it
    // fires the best-effort `unsubscribe-pane-output` request when the
    // parent stream is dropped. The rename signals to the linter that
    // we never read it; the guard's own [`Drop`] is the entire reason
    // it lives in this struct.
    _drop_guard: DropGuard,
    closed: bool,
}

impl PaneOutputStream {
    pub(crate) async fn open(
        transport: TransportClient,
        target: PaneTargetRef,
        start: PaneOutputStart,
    ) -> Result<Self> {
        let start = start.into_proto();
        let response = match target {
            PaneTargetRef::Slot(target) => {
                transport
                    .request(Request::SubscribePaneOutput(SubscribePaneOutputRequest {
                        target,
                        start,
                    }))
                    .await?
            }
            PaneTargetRef::Id { .. } => {
                crate::capabilities::require(&transport, &[CAPABILITY_SDK_PANE_BY_ID]).await?;
                transport
                    .request(Request::SubscribePaneOutputRef(
                        SubscribePaneOutputRefRequest { target, start },
                    ))
                    .await?
            }
        };

        let subscription_id = match response {
            Response::SubscribePaneOutput(response) => response.subscription_id,
            response => return Err(unexpected_response("subscribe-pane-output", response)),
        };

        // Stream polling is a sequence of independent operations. Do not
        // retain the already-consumed setup deadline in the returned stream.
        let transport = transport.reusable();
        let unsubscribe =
            Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest { subscription_id });
        let drop_guard = DropGuard::best_effort(transport.clone(), unsubscribe);

        Ok(Self {
            inner: PaneSubscription {
                transport,
                subscription_id,
                _drop_guard: drop_guard,
                closed: false,
            },
            pending: VecDeque::new(),
            poll_delay: POLL_INITIAL_DELAY,
        })
    }

    /// Returns the next chunk, awaiting daemon output if necessary.
    ///
    /// Returns `Ok(None)` once the daemon reports the subscription is no
    /// longer alive — for example after the pane closed and the daemon
    /// removed the subscription record. The drop-time best-effort
    /// unsubscribe still runs in that case.
    pub async fn next(&mut self) -> Result<Option<PaneOutputChunk>> {
        if let Some(chunk) = self.pop_pending_chunk() {
            return Ok(Some(chunk));
        }
        if self.inner.closed {
            return Ok(None);
        }

        loop {
            match self.refill_once().await? {
                RefillOutcome::Closed => {
                    self.inner.closed = true;
                    return Ok(None);
                }
                RefillOutcome::Filled => {
                    if let Some(chunk) = self.pop_pending_chunk() {
                        self.poll_delay = POLL_INITIAL_DELAY;
                        return Ok(Some(chunk));
                    }
                    let delay = self.poll_delay;
                    self.poll_delay = (self.poll_delay * 2).min(POLL_MAX_DELAY);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Drains any chunks that the daemon already has queued for this
    /// subscription. Returns an empty vec when no chunks were ready.
    ///
    /// `poll_once` performs exactly one
    /// [`PaneOutputCursorRequest`] round trip and never sleeps, which
    /// makes it the appropriate primitive for callers that want explicit
    /// control over their own backoff.
    pub async fn poll_once(&mut self) -> Result<Vec<PaneOutputChunk>> {
        let mut buffered: Vec<PaneOutputChunk> = self.pending.drain(..).collect();
        if self.inner.closed {
            return Ok(buffered);
        }

        match self.refill_once().await? {
            RefillOutcome::Closed => {
                self.inner.closed = true;
            }
            RefillOutcome::Filled => {
                buffered.extend(self.pending.drain(..));
                if buffered.iter().any(output_chunk_is_eof) {
                    self.inner.closed = true;
                }
            }
        }
        Ok(buffered)
    }

    async fn refill_once(&mut self) -> Result<RefillOutcome> {
        let request = Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id: self.inner.subscription_id,
            max_events: Some(PANE_OUTPUT_BATCH_SIZE),
        });

        match self.inner.transport.request(request).await {
            Ok(Response::PaneOutputCursor(cursor)) => {
                self.inner
                    .validate_response_subscription("pane-output-cursor", cursor.subscription_id)?;
                ingest_cursor(&mut self.pending, cursor.events);
                Ok(RefillOutcome::Filled)
            }
            Ok(Response::PaneOutputLag(lag)) => {
                self.inner
                    .validate_response_subscription("pane-output-lag", lag.subscription_id)?;
                self.pending
                    .push_back(PaneOutputChunk::Lag(PaneLagNotice::from_proto(lag.lag)));
                Ok(RefillOutcome::Filled)
            }
            Ok(response) => Err(unexpected_response("pane-output-cursor", response)),
            Err(error) if is_subscription_gone(&error) => Ok(RefillOutcome::Closed),
            Err(error) => Err(error),
        }
    }

    fn pop_pending_chunk(&mut self) -> Option<PaneOutputChunk> {
        let chunk = self.pending.pop_front()?;
        if output_chunk_is_eof(&chunk) {
            self.inner.closed = true;
        }
        Some(chunk)
    }
}

impl PaneSubscription {
    fn validate_response_subscription(
        &self,
        command: &'static str,
        response_id: PaneOutputSubscriptionId,
    ) -> Result<()> {
        if response_id == self.subscription_id {
            return Ok(());
        }
        Err(subscription_mismatch_error(
            command,
            self.subscription_id,
            response_id,
        ))
    }
}

fn subscription_mismatch_error(
    command: &'static str,
    expected: PaneOutputSubscriptionId,
    got: PaneOutputSubscriptionId,
) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "rmux daemon sent subscription id {} in `{command}` response for subscription {}",
        got.as_u64(),
        expected.as_u64()
    )))
}

fn ingest_cursor(target: &mut VecDeque<PaneOutputChunk>, events: Vec<PaneOutputEvent>) {
    target.reserve(events.len());
    for event in events {
        target.push_back(PaneOutputChunk::Bytes {
            sequence: event.sequence,
            bytes: event.bytes,
        });
    }
}

fn output_chunk_is_eof(chunk: &PaneOutputChunk) -> bool {
    matches!(chunk, PaneOutputChunk::Bytes { bytes, .. } if bytes.is_empty())
}

enum RefillOutcome {
    Filled,
    Closed,
}

fn is_subscription_gone(error: &RmuxError) -> bool {
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Server(message),
        } => message == "subscription not found" || message == "subscription receiver not found",
        _ => false,
    }
}

impl PaneLineStream {
    pub(crate) fn wrap(inner: PaneOutputStream) -> Self {
        Self {
            inner,
            line_buffer: Vec::new(),
            line_buffer_force_flushed: false,
            pending: VecDeque::new(),
        }
    }

    /// Returns the next line or lag notice, awaiting daemon output if
    /// necessary.
    ///
    /// Returns `Ok(None)` when the underlying subscription is gone. Any
    /// trailing partial-line bytes that were never terminated by `\n`
    /// are dropped at end-of-stream because the daemon never delivered a
    /// terminator — they did not represent a complete line.
    pub async fn next(&mut self) -> Result<Option<PaneLineItem>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Ok(Some(item));
            }
            match self.inner.next().await? {
                Some(PaneOutputChunk::Bytes { bytes, .. }) => {
                    split_lines_bounded(
                        &mut self.line_buffer,
                        &mut self.line_buffer_force_flushed,
                        &bytes,
                        &mut self.pending,
                    );
                }
                Some(PaneOutputChunk::Lag(notice)) => {
                    // Drop partial-line buffer because the byte stream is
                    // discontinuous after a lag — the next bytes may not
                    // begin at a line boundary, so concatenating them
                    // would produce a synthetic line.
                    self.line_buffer.clear();
                    self.line_buffer_force_flushed = false;
                    self.pending.push_back(PaneLineItem::Lag(notice));
                }
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
fn split_lines(buffer: &mut Vec<u8>, bytes: &[u8], out: &mut VecDeque<PaneLineItem>) {
    let mut force_flushed = false;
    split_lines_bounded(buffer, &mut force_flushed, bytes, out);
}

fn split_lines_bounded(
    buffer: &mut Vec<u8>,
    force_flushed: &mut bool,
    bytes: &[u8],
    out: &mut VecDeque<PaneLineItem>,
) {
    for byte in bytes {
        if *byte == b'\n' {
            if buffer.is_empty() && *force_flushed {
                *force_flushed = false;
                continue;
            }
            push_line(buffer, out);
            *force_flushed = false;
        } else {
            buffer.push(*byte);
            *force_flushed = false;
            if buffer.len() >= LINE_BUFFER_MAX {
                push_line(buffer, out);
                *force_flushed = true;
            }
        }
    }
}

fn push_line(buffer: &mut Vec<u8>, out: &mut VecDeque<PaneLineItem>) {
    let line_bytes = std::mem::take(buffer);
    out.push_back(PaneLineItem::Line {
        text: String::from_utf8_lossy(&line_bytes).into_owned(),
    });
}

impl std::fmt::Debug for PaneOutputStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneOutputStream")
            .field("closed", &self.inner.closed)
            .field("buffered_chunks", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for PaneLineStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneLineStream")
            .field("buffered_bytes", &self.line_buffer.len())
            .field("pending_items", &self.pending.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "streams_contract_tests.rs"]
mod streams_contract_tests;

#[cfg(test)]
#[path = "streams_tests.rs"]
mod streams_tests;
