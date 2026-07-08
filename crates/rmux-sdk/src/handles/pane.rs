//! Daemon-backed pane handle.
//!
//! A pane handle can address either an index slot or a stable [`PaneId`].
//! Slot handles preserve existing tmux-like `(session, window, pane)` behavior;
//! stable handles use by-id daemon routes where available and otherwise resolve
//! the id against the daemon's current view before issuing the request.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::events::streams::{PaneLineStream, PaneOutputStart, PaneOutputStream};
use crate::handles::split::SplitDirection;
use crate::transport::TransportClient;
use crate::{
    CollectedPaneOutput, InfoSnapshot, PaneId, PaneRef, PaneRenderStream, PaneSnapshot,
    PaneTextMatch, ProcessSpec, Result, RmuxEndpoint, RmuxError, TerminalSizeSpec,
};

#[path = "pane/capture_pane.rs"]
mod capture_pane;
#[path = "pane/foreground.rs"]
mod foreground;
#[path = "pane/info.rs"]
mod info;
#[path = "pane/input.rs"]
mod input;
#[path = "pane/lifecycle.rs"]
mod lifecycle;
#[path = "pane/options.rs"]
mod options;
#[path = "pane/snapshot.rs"]
mod snapshot;
#[path = "pane/spawn.rs"]
mod spawn;
#[path = "pane/split.rs"]
mod split;
#[path = "pane/split_builder.rs"]
mod split_builder;
#[path = "pane/state_events.rs"]
mod state_events;
#[path = "pane/target.rs"]
mod target;
#[path = "pane/title.rs"]
mod title;
#[path = "pane/waits.rs"]
mod waits;

pub use capture_pane::{PaneCapture, PaneCaptureBuilder};
pub use foreground::{ForegroundSource, ForegroundSources, ForegroundState};
use info::{current_pane_entry, current_pane_ref_for_id, pane_info_snapshot};
use input::{resize_to_size, send_key, send_text};
use lifecycle::{close_pane, respawn_pane};
use options::{get_option, set_option, unset_option};
use snapshot::pane_snapshot;
pub use spawn::PaneSpawnBuilder;
use split::split_pane;
pub use split_builder::PaneSplitBuilder;
pub use state_events::{
    PaneStateClosedReason, PaneStateEvent, PaneStateEventStream, PaneStateEventsOptions,
    PaneStateOption,
};
pub(crate) use target::is_already_closed_pane_error;
use title::{get_title, set_title};

pub(crate) async fn resolve_pane_ref_for_id(
    transport: &TransportClient,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<Option<PaneRef>> {
    current_pane_ref_for_id(transport, session_name, pane_id).await
}

/// Result of consuming a [`Pane`] handle with [`Pane::close`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneCloseOutcome {
    /// The daemon killed the addressed pane.
    Closed {
        /// The pane target consumed by the close call.
        target: PaneRef,
        /// Whether the pane removal also destroyed its window.
        window_destroyed: bool,
    },
    /// The addressed pane was already absent by the time close ran.
    AlreadyClosed {
        /// The stale target consumed by the close call.
        target: PaneRef,
    },
}

/// Process and policy fields for [`Pane::respawn`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneRespawnOptions {
    /// Whether a running pane should be killed before respawning.
    pub kill: bool,
    /// Optional working-directory override for the new process.
    pub start_directory: Option<PathBuf>,
    /// Process argv and per-spawn environment overrides.
    pub process: ProcessSpec,
    /// Optional keep-dead-pane policy applied before respawn.
    pub keep_alive_on_exit: Option<bool>,
}

/// Result of a pane-local option mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneOptionMutation {
    /// Stable pane id resolved by the daemon.
    pub pane_id: PaneId,
    /// Canonical option name.
    pub name: String,
    /// Exact explicit value before the mutation.
    pub old_value: Option<String>,
    /// Exact explicit value after the mutation.
    pub new_value: Option<String>,
    /// Whether the explicit value changed.
    pub changed: bool,
}

/// Opaque handle for one daemon pane slot.
///
/// A pane handle addresses a `(session, window, pane)` triple rather than
/// caching a `PaneId`. Every operation resolves that slot against the
/// daemon's current state, so:
///
/// * linked windows and grouped sessions keep returning the same stable
///   `%N` identity through every sibling view, and
/// * stale handles for an already-closed pane resolve to typed
///   `None`/empty results — never to a panic and never to a `PaneId` from
///   a prior epoch.
///
/// The handle deliberately exposes no `current_revision()` accessor.
/// Revision values are only observable through
/// [`PaneSnapshot::revision`] on a freshly captured snapshot, or through
/// the revision-carrying [`PaneEvent`](crate::PaneEvent) variants emitted
/// over a control-mode subscription.
#[derive(Clone)]
pub struct Pane {
    target: PaneRef,
    stable_id: Option<PaneId>,
    endpoint: RmuxEndpoint,
    default_timeout: Option<Duration>,
    transport: TransportClient,
}

impl Pane {
    pub(crate) fn new(
        target: PaneRef,
        endpoint: RmuxEndpoint,
        default_timeout: Option<Duration>,
        transport: TransportClient,
    ) -> Self {
        Self {
            target,
            stable_id: None,
            endpoint,
            default_timeout,
            transport,
        }
    }

    pub(crate) fn new_by_id(
        target: PaneRef,
        pane_id: PaneId,
        endpoint: RmuxEndpoint,
        default_timeout: Option<Duration>,
        transport: TransportClient,
    ) -> Self {
        Self {
            target,
            stable_id: Some(pane_id),
            endpoint,
            default_timeout,
            transport,
        }
    }

    /// Returns the exact protocol-owned pane target addressed by this
    /// handle.
    #[must_use]
    pub const fn target(&self) -> &PaneRef {
        &self.target
    }

    /// Returns the endpoint that was resolved when this handle was created.
    #[must_use]
    pub const fn endpoint(&self) -> &RmuxEndpoint {
        &self.endpoint
    }

    /// Returns the default timeout configured on the parent facade.
    #[must_use]
    pub const fn configured_default_timeout(&self) -> Option<Duration> {
        self.default_timeout
    }

    pub(crate) const fn transport(&self) -> &TransportClient {
        &self.transport
    }

    pub(crate) fn proto_target_ref(&self) -> rmux_proto::PaneTargetRef {
        match self.stable_id {
            Some(pane_id) => {
                rmux_proto::PaneTargetRef::by_id(self.target.session_name.clone(), pane_id)
            }
            None => rmux_proto::PaneTargetRef::slot(self.target.to_proto()),
        }
    }

    pub(crate) const fn is_stable_id(&self) -> bool {
        self.stable_id.is_some()
    }

    /// Subscribes to the live raw pane output as a typed byte stream.
    ///
    /// Setup performs one `subscribe-pane-output` round trip and is
    /// fallible: a stale pane slot, a transport failure, or a refused
    /// daemon capability propagates as [`crate::RmuxError`].
    ///
    /// The returned [`PaneOutputStream`] preserves arbitrary bytes,
    /// pairs every chunk with the daemon's monotonic per-pane sequence,
    /// and surfaces any retained-output gaps as
    /// [`PaneOutputChunk::Lag`](crate::PaneOutputChunk::Lag) without ever
    /// converting raw bytes through `String::from_utf8_lossy`. Dropping
    /// the stream emits exactly one best-effort
    /// `unsubscribe-pane-output` request; if the unsubscribe is refused,
    /// late, or the transport is already gone the drop never closes the
    /// pane, its window/session/process, or the daemon itself.
    pub async fn output_stream(&self) -> Result<PaneOutputStream> {
        self.output_stream_starting_at(PaneOutputStart::Now).await
    }

    /// Subscribes to the live raw pane output, anchoring the cursor at
    /// the requested start position.
    ///
    /// See [`Self::output_stream`] for setup, drop, and lag semantics.
    pub async fn output_stream_starting_at(
        &self,
        start: PaneOutputStart,
    ) -> Result<PaneOutputStream> {
        PaneOutputStream::open(self.transport.clone(), self.proto_target_ref(), start).await
    }

    /// Collects bounded raw pane output bytes until the pane process exits.
    ///
    /// Collection starts at the live output cursor, retains at most
    /// `max_bytes`, and keeps waiting for pane exit even after the cap is
    /// reached. Returned bytes are raw pane-output bytes; lag notices are
    /// reported on the returned [`CollectedPaneOutput`] and are not spliced
    /// into the byte buffer.
    pub async fn collect_output_until_exit(&self, max_bytes: usize) -> Result<CollectedPaneOutput> {
        crate::extract::collect_output_until_exit(self, max_bytes).await
    }

    /// Collects bounded raw pane output from the requested stream start until
    /// the pane process exits.
    ///
    /// See [`Self::collect_output_until_exit`] for cap, lag, and byte
    /// preservation semantics.
    pub async fn collect_output_until_exit_starting_at(
        &self,
        start: PaneOutputStart,
        max_bytes: usize,
    ) -> Result<CollectedPaneOutput> {
        crate::extract::collect_output_until_exit_starting_at(self, start, max_bytes).await
    }

    /// Subscribes to the live pane output rendered into UTF-8 lines.
    ///
    /// Setup is fallible (see [`Self::output_stream`]). Beyond the raw
    /// stream the line stream applies two well-isolated transformations:
    /// it splits on the LF byte `b'\n'` and runs each completed line
    /// through `String::from_utf8_lossy`, replacing every byte that is
    /// not valid UTF-8 with the Unicode replacement character `U+FFFD`.
    /// Bytes between LFs stay buffered until the next LF arrives, and a
    /// daemon-side lag drops the in-flight partial line; both
    /// transformations are documented in detail on the
    /// [`crate::events::streams`] module. Drop semantics match
    /// [`Self::output_stream`].
    pub async fn line_stream(&self) -> Result<PaneLineStream> {
        self.line_stream_starting_at(PaneOutputStart::Now).await
    }

    /// Subscribes to rendered output lines, anchoring the cursor at the
    /// requested start position.
    pub async fn line_stream_starting_at(&self, start: PaneOutputStart) -> Result<PaneLineStream> {
        let inner = self.output_stream_starting_at(start).await?;
        Ok(PaneLineStream::wrap(inner))
    }

    /// Opens a minimal render stream that emits snapshots after output.
    ///
    /// The implementation is output-driven with debounce and revision
    /// filtering. It avoids fixed-rate blind refresh loops but is not a
    /// daemon-native snapshot-diff stream.
    pub async fn render_stream(&self) -> Result<PaneRenderStream> {
        PaneRenderStream::open(self.clone()).await
    }

    /// Returns the live daemon pane identity for this slot, when it is
    /// currently listed.
    ///
    /// Returns `Ok(None)` (rather than an error) for a stale slot, mirroring
    /// the [`Window`](super::Window)-handle stale-slot semantics.
    pub async fn id(&self) -> Result<Option<PaneId>> {
        if let Some(pane_id) = self.stable_id {
            let current =
                current_pane_ref_for_id(&self.transport, &self.target.session_name, pane_id)
                    .await?;
            return Ok(current.map(|_| pane_id));
        }
        Ok(current_pane_entry(&self.transport, &self.target)
            .await?
            .map(|entry| entry.pane_id))
    }

    /// Checks whether this exact pane slot is currently listed by the
    /// daemon.
    pub async fn exists(&self) -> Result<bool> {
        Ok(self.id().await?.is_some())
    }

    /// Returns a sticky info snapshot scoped to this pane's session,
    /// window, and pane.
    ///
    /// The snapshot is assembled from live `list-sessions`,
    /// `list-windows`, `list-panes`, and `display-message -p` responses so
    /// pane process state — running pid, exit state, geometry — reflects
    /// the daemon's current view rather than any handle-cached value.
    /// Stale slots return what is still observable: a session-only
    /// snapshot when the window or pane is gone, or an empty snapshot
    /// when the session itself is gone.
    pub async fn info(&self) -> Result<InfoSnapshot> {
        match self.stable_id {
            Some(pane_id) => {
                let Some(target) =
                    current_pane_ref_for_id(&self.transport, &self.target.session_name, pane_id)
                        .await?
                else {
                    return Ok(InfoSnapshot::default());
                };
                pane_info_snapshot(&self.transport, &target).await
            }
            None => pane_info_snapshot(&self.transport, &self.target).await,
        }
    }

    /// Captures the live pane grid as a [`PaneSnapshot`].
    ///
    /// The captured grid is read directly from the daemon's live
    /// rmux-core screen — the same in-memory grid that the crate-private
    /// terminal parser feeds from PTY output — so dimensions, cursor
    /// state, and per-cell glyph/attribute/colour data round-trip without
    /// any `capture-pane -p` text reconstruction step. Wide-glyph padding
    /// is preserved as padding cells in the row-major layout, raw bytes
    /// that are not valid UTF-8 stay isolated to the cell text payload
    /// rather than reaching helper output, and the daemon-derived
    /// [`revision`](PaneSnapshot::revision) is non-zero for a live pane
    /// and changes whenever any observable pane field mutates — output,
    /// resize, clear, exit. Stale slots resolve to a default empty
    /// snapshot whose revision is `0`, distinct from any prior live
    /// revision.
    pub async fn snapshot(&self) -> Result<PaneSnapshot> {
        pane_snapshot(self).await
    }

    /// Starts a daemon `capture-pane` request builder.
    pub fn capture_pane(&self) -> PaneCaptureBuilder<'_> {
        PaneCaptureBuilder::new(self)
    }

    /// Captures a fresh snapshot and searches its rendered visible text for
    /// the first literal match.
    ///
    /// This is a lossy rendered-text helper built from
    /// [`PaneSnapshot::visible_lines`]. It does not inspect raw output bytes
    /// and does not use any daemon/core regex search surface.
    pub async fn find_text(&self, text: impl AsRef<str>) -> Result<Option<PaneTextMatch>> {
        crate::extract::find_text(self, text.as_ref().to_owned()).await
    }

    /// Captures a fresh snapshot and returns every literal rendered-text
    /// match, including overlapping matches on the same visible line.
    ///
    /// See [`Self::find_text`] for rendered-text and coordinate semantics.
    pub async fn find_text_all(&self, text: impl AsRef<str>) -> Result<Vec<PaneTextMatch>> {
        crate::extract::find_text_all(self, text.as_ref().to_owned()).await
    }

    /// Sends literal UTF-8 text bytes to this pane through the daemon.
    ///
    /// The payload is not interpreted as key names, does not expand tmux
    /// formats, and does not receive an implicit trailing newline. Use
    /// [`send_key`](Self::send_key) when a tmux key token such as `Enter`
    /// should be interpreted as a key press.
    pub async fn send_text(&self, text: impl AsRef<str>) -> Result<()> {
        send_text(self, text.as_ref()).await
    }

    /// Sends one tmux-compatible key token to this pane through the daemon.
    ///
    /// Tokens keep the daemon's existing `send-keys` semantics: known key
    /// names such as `Enter` are encoded as keys, while ordinary text tokens
    /// are forwarded as their bytes by the server.
    pub async fn send_key(&self, key: impl Into<String>) -> Result<()> {
        send_key(self, key.into()).await
    }

    /// Requests an absolute pane size through the daemon.
    ///
    /// Only dimensions that differ from the daemon's current pane details are
    /// sent. The daemon still applies normal `resize-pane` layout rules, so
    /// linked panes, borders, and neighboring panes can constrain the final
    /// geometry. No pane identity is cached by this handle.
    pub async fn resize(&self, size: TerminalSizeSpec) -> Result<()> {
        resize_to_size(self, size).await
    }

    /// Sets this pane's UX title label.
    ///
    /// Titles are labels for humans and UI surfaces. They are not technical
    /// identity; use [`Self::id`] and [`Session::pane_by_id`](super::Session::pane_by_id)
    /// for stable addressing.
    pub async fn set_title(&self, title: impl Into<String>) -> Result<()> {
        set_title(self, title.into()).await
    }

    /// Returns this pane's current UX title label when the pane still exists.
    pub async fn title(&self) -> Result<Option<String>> {
        get_title(self).await
    }

    /// Sets a pane-local option and returns the exact mutation outcome.
    pub async fn set_option(
        &self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<PaneOptionMutation> {
        set_option(self, name.into(), value.into()).await
    }

    /// Returns the exact pane-local explicit value for an option.
    pub async fn option(&self, name: impl Into<String>) -> Result<Option<String>> {
        get_option(self, name.into()).await
    }

    /// Removes a pane-local explicit option and returns the exact mutation outcome.
    pub async fn unset_option(&self, name: impl Into<String>) -> Result<PaneOptionMutation> {
        unset_option(self, name.into()).await
    }

    /// Returns best-effort foreground process state for this pane.
    pub async fn foreground_state(&self) -> Result<Option<ForegroundState>> {
        foreground::foreground_state(self)
            .await
            .map(|state| state.map(|(_, _, foreground)| foreground))
    }

    /// Opens a long-poll stream of pane title, option, close, and optional foreground events.
    pub async fn state_events(
        &self,
        options: PaneStateEventsOptions,
    ) -> Result<PaneStateEventStream> {
        state_events::PaneStateEventStream::open(self, options).await
    }

    /// Consumes this handle and kills the addressed pane through the daemon.
    ///
    /// A stale handle is treated as an idempotent no-op and returns
    /// [`PaneCloseOutcome::AlreadyClosed`]. Dropping a [`Pane`] handle remains
    /// inert; this consuming method is the SDK operation that explicitly
    /// closes the pane slot and its process.
    pub async fn close(self) -> Result<PaneCloseOutcome> {
        close_pane(self).await
    }

    /// Consumes this handle without sending any daemon request.
    ///
    /// Detaching an SDK handle is equivalent to dropping it: the addressed
    /// pane slot, process, subscriptions owned elsewhere, and daemon state are
    /// left untouched. Use [`Self::close`] when the pane itself should be
    /// killed.
    pub fn detach(self) {}

    /// Splits this pane and returns a handle for the freshly spawned pane.
    ///
    /// The direction names where the new pane lands relative to this one:
    /// `Right`/`Left` create a side-by-side arrangement (vertical divider),
    /// `Up`/`Down` create a stacked arrangement (horizontal divider).
    /// `Left` and `Up` map to tmux's `-b` flag — the new pane is inserted
    /// *before* this one on the chosen axis.
    ///
    /// For handles created by [`Session::pane_by_id`](crate::Session::pane_by_id),
    /// the SDK resolves the stable pane id to the daemon's current slot before
    /// issuing the split request. Unlike input, snapshot, waits, and streams,
    /// split is therefore not yet an atomic daemon-side by-id operation.
    pub async fn split(&self, direction: SplitDirection) -> Result<Self> {
        let target = self.current_target().await?;
        let new_target = split_pane(&self.transport, &target, direction).await?;
        Ok(Self::new(
            new_target,
            self.endpoint.clone(),
            self.default_timeout,
            self.transport.clone(),
        ))
    }

    /// Starts building an atomic split that may choose the new pane process.
    ///
    /// Unlike `self.split(direction).await?.spawn(command).await?`, this
    /// builder sends the process specification with the split request, so the
    /// daemon never creates the new pane with an intermediate default shell
    /// that is immediately replaced.
    ///
    /// On a stable-id pane handle, this builder has the same targeting
    /// limitation as [`Self::split`]: the id is resolved to the current slot
    /// before the split request is sent.
    pub fn split_with(&self, direction: SplitDirection) -> PaneSplitBuilder<'_> {
        PaneSplitBuilder::new(self, direction)
    }

    /// Respawns the process in this pane slot through the daemon.
    ///
    /// The addressed slot and stable `%N`/[`PaneId`] are preserved by the
    /// daemon. `options.kill` mirrors `respawn-pane -k`: a running process is
    /// rejected unless that flag is set, while a dead pane can be respawned
    /// without it. The daemon resets the pane transcript, parser state,
    /// scrollback, and retained output before exposing output from the fresh
    /// lifecycle generation.
    pub async fn respawn(&self, options: PaneRespawnOptions) -> Result<PaneRef> {
        respawn_pane(self, options).await
    }

    /// Starts a structured respawn builder for this pane.
    ///
    /// `spawn(argv)` is an argv-oriented wrapper around [`Self::respawn`]:
    /// it does not send text to an interactive shell and it does not append a
    /// newline. A running process is rejected by default; call
    /// [`PaneSpawnBuilder::kill_existing`] when replacement is intentional.
    pub fn spawn<I, S>(&self, command: I) -> PaneSpawnBuilder<'_>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        PaneSpawnBuilder::argv(self, command.into_iter().map(Into::into).collect())
    }

    /// Starts an explicit shell-command respawn builder for this pane.
    ///
    /// This is the intentional `$SHELL -c` path. Use [`Self::spawn`] when the
    /// process should be represented as structured argv without shell
    /// interpretation.
    pub fn shell(&self, command: impl Into<String>) -> PaneSpawnBuilder<'_> {
        PaneSpawnBuilder::shell(self, command.into())
    }

    pub(crate) async fn current_target(&self) -> Result<PaneRef> {
        let Some(pane_id) = self.stable_id else {
            return Ok(self.target.clone());
        };
        current_pane_ref_for_id(&self.transport, &self.target.session_name, pane_id)
            .await?
            .ok_or_else(|| RmuxError::pane_not_found(self.target.session_name.clone(), pane_id))
    }
}

impl fmt::Debug for Pane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pane")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "pane/state_events_tests.rs"]
mod state_events_tests;

#[cfg(test)]
#[path = "pane/tests.rs"]
mod tests;
