//! SDK pane event vocabulary.
//!
//! The [`PaneEvent`] enum models the tmux-compatible control-mode line
//! vocabulary the rmux daemon emits to attached `-C`/`-CC` clients. The
//! types here are *inert* DTOs: they do not parse the wire bytes, hold
//! channel handles, or run state machines. They exist so SDK consumers can
//! receive a typed projection of the daemon's control-mode stream without
//! pulling in `rmux-core`, `rmux-server`, `rmux-client`, or `rmux-pty`.
//!
//! ## Output sequencing semantics
//!
//! The order in which a daemon emits these events is intentionally
//! observable, because `rmux-server`'s control loop in
//! `crates/rmux-server/src/control.rs` matches the tmux compatibility
//! contract. Consumers that resequence events MUST preserve these rules:
//!
//! * **Command stdout flushes before `%end`/`%error`.** When an active
//!   command block resolves, any [`PaneCommandSummary::stdout`]
//!   bytes are written into the output queue *before* the trailing
//!   [`%end` or `%error` guard line](PaneCommandSummary). The guard line
//!   carries the same `command_number`/`timestamp` as the matching
//!   `%begin`. Any [`PaneEvent::Output`] or [`PaneEvent::ExtendedOutput`]
//!   that has already arrived for a non-paused pane is also drained ahead
//!   of the guard line so the transcript respects causal order.
//! * **Notifications and exits defer until active command blocks close.**
//!   While a `%begin`/`%end` block is in flight, any
//!   [`PaneEvent::Notification`] or [`PaneEvent::Exit`] the server would
//!   normally emit is queued and replayed only after the command block
//!   has completed. A deferred [`PaneEvent::Exit`] additionally waits for
//!   all queued notifications to flush, so the final transcript ends with
//!   `%message` lines, then `%exit`.
//! * **EOF and empty input emit a bare `%exit`.** When the client closes
//!   stdin (or sends an empty line), the server emits
//!   [`PaneEvent::Exit`] with [`PaneExitReason::Bare`] — this is the
//!   `%exit` line with no reason text and no preceding guard tuple, and
//!   it is the canonical way the control transcript terminates.
//! * **Lag precedes the matching disconnect.** A [`PaneEvent::Lag`]
//!   indicates the per-pane broadcast receiver skipped frames before
//!   those frames reached the control output queue. When an SDK timeline
//!   records the following transport teardown, the trailing
//!   [`PaneEvent::Disconnect`] carries
//!   [`PaneDisconnectReason::TooFarBehind`] and the same `pane_id` in its
//!   optional attribution field. This is distinct from the daemon's aged
//!   output-queue path, which writes `%exit too far behind` after a queued
//!   output block waits past the tmux-compatible maximum age and has no
//!   reliable pane attribution.
//!
//! These rules are exercised by the JSON/bincode roundtrip tests in
//! `crates/rmux-sdk/tests/events.rs`, which cover every variant including
//! raw byte payloads and [`PaneId`] identity fields.

use serde::{Deserialize, Serialize};

use crate::types::PaneId;

/// Tmux-compatible control-mode pane event vocabulary surfaced by the SDK.
///
/// The enum is externally tagged for serde, so the JSON projection of
/// each variant is `{"<kebab-case-tag>": {...}}` (or `"<tag>"` for unit
/// variants). External tagging is what `bincode` supports natively, so
/// the same encoding round-trips through both `serde_json` and
/// `bincode`.
///
/// Marked `#[non_exhaustive]` because the daemon vocabulary is a moving
/// target — added variants must not break downstream pattern matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PaneEvent {
    /// `%output %<pane> <octal-bytes>` pane stdout payload.
    ///
    /// `bytes` carries the *decoded* pane bytes (the SDK transport layer
    /// reverses the `\NNN` octal escaping done on the wire). The payload is
    /// arbitrary binary, including NUL and bytes outside ASCII; consumers
    /// must not assume UTF-8.
    Output {
        /// Originating pane identity (`%N`).
        pane_id: PaneId,
        /// Raw decoded pane bytes.
        bytes: Vec<u8>,
    },
    /// `%extended-output %<pane> <age_ms> : <octal-bytes>` pane payload with
    /// the queue residency age the daemon recorded for the chunk.
    ///
    /// The `age_ms` value comes from the daemon's
    /// `Instant::now().duration_since(received_at)` measurement at flush
    /// time and is bounded to fit in `u64` milliseconds.
    ExtendedOutput {
        /// Originating pane identity (`%N`).
        pane_id: PaneId,
        /// Milliseconds the chunk waited in the daemon output queue.
        age_ms: u64,
        /// Raw decoded pane bytes.
        bytes: Vec<u8>,
    },
    /// `%pause %<pane>` — the daemon paused emitting output for this pane
    /// because the buffered control-mode bytes crossed the high watermark.
    Pause {
        /// Paused pane identity.
        pane_id: PaneId,
    },
    /// `%continue %<pane>` — the daemon resumed emitting output for this
    /// pane after the buffered byte count fell back below the low
    /// watermark.
    Continue {
        /// Resumed pane identity.
        pane_id: PaneId,
    },
    /// Internal lag signal: the broadcast channel feeding `pane_id`
    /// skipped frames before the SDK could observe them.
    ///
    /// Broadcast lag is terminal for this subscription. If the same SDK
    /// timeline also records the transport close, this event MUST precede
    /// a matching [`PaneEvent::Disconnect`] with
    /// [`PaneDisconnectReason::TooFarBehind`] and
    /// `pane_id: Some(<same pane>)`. A plain `%exit too far behind` line
    /// without a preceding `Lag` belongs to the aged output-queue path
    /// instead.
    Lag {
        /// Pane whose broadcast channel lagged.
        pane_id: PaneId,
    },
    /// Connection-level disconnect with a structured reason.
    ///
    /// Disconnect is distinct from [`PaneEvent::Exit`]: an `Exit` carries a
    /// human-readable `%exit` reason emitted by the server, while a
    /// `Disconnect` is the SDK's projection of the transport teardown that
    /// follows. The two are emitted in pairs for graceful exits and as a
    /// single `Disconnect` for ungraceful socket loss. `pane_id` is set
    /// only when the disconnect can be attributed to a specific pane, such
    /// as the `Lag`/`TooFarBehind` pair.
    Disconnect {
        /// Pane that caused the disconnect, when the daemon can identify
        /// one.
        #[serde(default)]
        pane_id: Option<PaneId>,
        /// Structured disconnect reason.
        reason: PaneDisconnectReason,
    },
    /// `%exit [reason]` — the daemon is closing the control-mode session.
    ///
    /// Reason text is empty for the bare `%exit` form emitted on EOF and
    /// empty-input close paths; see [`PaneExitReason`].
    Exit {
        /// Structured `%exit` reason.
        reason: PaneExitReason,
    },
    /// A pane has closed (its underlying process exited or it was killed).
    ///
    /// On the wire the daemon broadcasts pane lifecycle events through the
    /// `%pane-mode-changed`/`%window-pane-changed` notification family;
    /// the SDK projects pane termination into this dedicated variant so
    /// consumers can release per-pane resources without parsing
    /// notification text.
    Close {
        /// Closed pane identity.
        pane_id: PaneId,
    },
    /// A write or session mutation was refused because the client is
    /// read-only or otherwise lacks permission.
    ///
    /// Mirrors the `client is read-only` server-side refusal in
    /// `crates/rmux-server/src/server_access.rs` and the `read-only`
    /// client-flag tracked in `crates/rmux-server/src/client_flags.rs`.
    PermissionDenied {
        /// Pane the refusal targeted, when the operation was pane-scoped.
        pane_id: Option<PaneId>,
        /// Permission scope that produced the refusal.
        scope: PanePermissionScope,
        /// Human-readable refusal message recorded by the daemon.
        reason: String,
    },
    /// `%message <text>` daemon notification (and the broader
    /// `%notification`-style line family).
    Notification(PaneNotification),
    /// Summary of a `%begin`/`%end`/`%error` command block.
    CommandSummary(PaneCommandSummary),
}

/// Structured reason carried by [`PaneEvent::Disconnect`].
///
/// Marked `#[non_exhaustive]` so new transport-level disconnect causes can
/// be modelled without breaking downstream pattern matches. Externally
/// tagged for serde, so the encoding round-trips through both
/// `serde_json` and `bincode`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PaneDisconnectReason {
    /// The stream became too far behind to continue.
    ///
    /// With the `pane_id` field set on [`PaneEvent::Disconnect`], this
    /// follows a preceding [`PaneEvent::Lag`] for the same pane. With no
    /// pane attribution, this can represent the daemon's aged output-queue
    /// path that writes `%exit too far behind` before closing.
    TooFarBehind,
    /// The daemon is shutting down gracefully.
    ServerShutdown,
    /// The deferred control-mode notification queue exceeded its bound.
    NotificationOverflow,
    /// The transport closed without a `%exit` line (raw socket loss).
    TransportClosed,
    /// Any other disconnect reason carried verbatim.
    Other {
        /// Human-readable reason text.
        reason: String,
    },
}

/// Structured reason carried by [`PaneEvent::Exit`].
///
/// `Bare` denotes the canonical `%exit\n` form emitted on EOF / empty
/// input and on server shutdown. `WithReason` carries the trailing reason
/// text from `%exit <reason>\n`; the daemon uses this when it ends a client
/// for a reason of its own (e.g. `%exit too far behind`). Externally tagged for
/// serde, so the encoding round-trips through both `serde_json` and
/// `bincode`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PaneExitReason {
    /// Bare `%exit\n` line. Emitted on EOF, on empty-input close and on
    /// server shutdown.
    Bare,
    /// `%exit <reason>\n` line carrying daemon-supplied reason text.
    WithReason {
        /// Trailing reason text from the wire form.
        reason: String,
    },
}

/// Permission scope that produced a [`PaneEvent::PermissionDenied`].
///
/// Marked `#[non_exhaustive]` so additional permission categories (such as
/// per-window or per-client policies) can be added without breaking
/// downstream pattern matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PanePermissionScope {
    /// Client carries the `read-only` flag and may not mutate state.
    ReadOnlyClient,
    /// Some other permission scope refused the operation. This keeps the
    /// SDK vocabulary honest until the daemon exposes more precise public
    /// permission categories.
    Other,
}

/// `%message`-style notification carried by [`PaneEvent::Notification`].
///
/// `text` is the already-decoded human-readable message text (the SDK
/// transport layer reverses the daemon's `encode_paste_bytes` escaping).
/// `pane_id` is set for pane-scoped notifications and omitted for
/// session/server-scoped lines such as `%sessions-changed`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneNotification {
    /// Pane the notification is scoped to, when applicable.
    #[serde(default)]
    pub pane_id: Option<PaneId>,
    /// Decoded notification text.
    #[serde(default)]
    pub text: String,
}

impl PaneNotification {
    /// Creates a session-scoped notification with no pane context.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            pane_id: None,
            text: text.into(),
        }
    }

    /// Creates a pane-scoped notification.
    #[must_use]
    pub fn for_pane(pane_id: PaneId, text: impl Into<String>) -> Self {
        Self {
            pane_id: Some(pane_id),
            text: text.into(),
        }
    }
}

/// Status of a completed control-mode command block.
///
/// The daemon's trailing guard line is either `%end` or `%error`; command
/// output and error text have already been flushed as stdout bytes before
/// that guard line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PaneCommandStatus {
    /// The trailing guard line was `%end`.
    #[default]
    End,
    /// The trailing guard line was `%error`.
    Error,
}

/// Summary of a `%begin`/`%end`/`%error` control-mode command block.
///
/// `timestamp`, `command_number`, and `flags` mirror the guard tuple
/// emitted by the daemon's
/// [`format_guard_line`](rmux_proto::format_guard_line). `status`
/// identifies the trailing guard kind. `stdout` carries every decoded byte
/// the daemon flushed *before* that guard line, including parse or command
/// error text for `%error` blocks.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneCommandSummary {
    /// Unix epoch seconds reported by `%begin`.
    pub timestamp: i64,
    /// Monotonic command number inside the control session.
    pub command_number: u64,
    /// `flags` byte from the guard tuple. Always `1` for the v1 daemon.
    pub flags: u8,
    /// Trailing guard status: `%end` or `%error`.
    #[serde(default)]
    pub status: PaneCommandStatus,
    /// Decoded command stdout, captured between `%begin` and the trailing
    /// guard line. May be empty for commands with no output. For `%error`,
    /// this also includes daemon-written error text.
    #[serde(default)]
    pub stdout: Vec<u8>,
}

impl PaneCommandSummary {
    /// Creates a successful command summary with the supplied stdout.
    #[must_use]
    pub fn success(timestamp: i64, command_number: u64, flags: u8, stdout: Vec<u8>) -> Self {
        Self {
            timestamp,
            command_number,
            flags,
            status: PaneCommandStatus::End,
            stdout,
        }
    }

    /// Creates a failed command summary whose trailing guard was `%error`.
    #[must_use]
    pub fn failure(timestamp: i64, command_number: u64, flags: u8, stdout: Vec<u8>) -> Self {
        Self {
            timestamp,
            command_number,
            flags,
            status: PaneCommandStatus::Error,
            stdout,
        }
    }

    /// Returns `true` when the trailing guard line was `%end`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status == PaneCommandStatus::End
    }
}
