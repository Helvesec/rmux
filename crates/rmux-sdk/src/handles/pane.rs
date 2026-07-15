//! Daemon-backed pane handle.
//!
//! A pane handle can address either an index slot or a stable [`PaneId`].
//! Slot handles preserve existing tmux-like `(session, window, pane)` behavior;
//! stable handles use by-id daemon routes where available and otherwise resolve
//! the id against the daemon's current view before issuing the request.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::handles::split::SplitDirection;
use crate::transport::{OperationDeadline, TransportClient};
use crate::{PaneId, PaneRef, ProcessSpec, Result, RmuxEndpoint, RmuxError, TerminalSizeSpec};

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
#[path = "pane/output.rs"]
mod output;
#[path = "pane/queries.rs"]
mod queries;
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
use info::current_pane_ref_for_id;
use input::{resize_to_size, send_key, send_text};
use lifecycle::{close_pane, respawn_pane};
use options::{get_option, set_option, unset_option};
pub use spawn::PaneSpawnBuilder;
use split::split_pane;
pub use split_builder::PaneSplitBuilder;
pub use state_events::{
    PaneStateClosedReason, PaneStateEvent, PaneStateEventStream, PaneStateEventsOptions,
    PaneStateOption,
};
pub(crate) use target::is_already_closed_pane_error;
use target::stale_slot_error;
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

/// Opaque handle for one daemon pane slot or stable pane identity.
///
/// Slot handles address a `(session, window, pane)` triple. Handles created
/// by `pane_by_id` retain a stable `PaneId`; identity lookup, snapshots, and
/// output/render stream opening resolve its current session dynamically,
/// preferring the session view from which the handle originated. This means:
///
/// * linked windows and grouped sessions keep returning the same stable
///   `%N` identity through every sibling view, and
/// * stable-id render and snapshot handles follow inter-session pane moves,
///   while slot handles remain scoped to their original slot, and
/// * stale handles for an already-closed pane resolve to typed
///   `None`/empty results — never to a panic and never to a `PaneId` from
///   a prior epoch.
///
/// The handle deliberately exposes no `current_revision()` accessor.
/// Revision values are only observable through
/// [`PaneSnapshot::revision`](crate::PaneSnapshot::revision) on a freshly
/// captured snapshot, or through
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
        let transport = transport.with_default_timeout(
            crate::bootstrap::discovery::resolve_timeout(None, default_timeout),
        );
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
        let transport = transport.with_default_timeout(
            crate::bootstrap::discovery::resolve_timeout(None, default_timeout),
        );
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

    pub(crate) fn begin_operation_handle(&self) -> Self {
        let mut pane = self.clone();
        pane.transport = pane.transport.begin_operation();
        pane
    }

    pub(crate) fn begin_operation_handle_with_timeout(
        &self,
        per_operation_timeout: Option<Duration>,
    ) -> Self {
        if self.transport.operation_deadline().is_some() {
            return self.clone();
        }
        let timeout = crate::bootstrap::discovery::resolve_timeout(
            per_operation_timeout,
            self.default_timeout,
        );
        let mut pane = self.clone();
        pane.transport = pane
            .transport
            .with_default_timeout(timeout)
            .begin_operation();
        pane
    }

    pub(crate) fn with_operation_deadline(&self, deadline: OperationDeadline) -> Self {
        let mut pane = self.clone();
        pane.transport = pane.transport.with_operation_deadline(deadline);
        pane
    }

    pub(crate) fn proto_target_ref(&self) -> rmux_proto::PaneTargetRef {
        match self.stable_id {
            Some(pane_id) => {
                rmux_proto::PaneTargetRef::by_id(self.target.session_name.clone(), pane_id)
            }
            None => rmux_proto::PaneTargetRef::slot(self.target.to_proto()),
        }
    }

    pub(crate) async fn resolved_proto_target_ref(
        &self,
    ) -> Result<Option<rmux_proto::PaneTargetRef>> {
        if let Some(pane_id) = self.stable_id {
            return Ok(current_pane_ref_for_id(
                &self.transport,
                &self.target.session_name,
                pane_id,
            )
            .await?
            .map(|target| rmux_proto::PaneTargetRef::by_id(target.session_name, pane_id)));
        }
        Ok(self.id().await?.map(|pane_id| {
            rmux_proto::PaneTargetRef::by_id(self.target.session_name.clone(), pane_id)
        }))
    }

    pub(crate) async fn required_resolved_proto_target_ref(
        &self,
    ) -> Result<rmux_proto::PaneTargetRef> {
        self.resolved_proto_target_ref().await?.ok_or_else(|| {
            self.stable_id.map_or_else(
                || stale_slot_error(&self.target),
                |pane_id| RmuxError::pane_not_found(self.target.session_name.clone(), pane_id),
            )
        })
    }

    pub(crate) const fn is_stable_id(&self) -> bool {
        self.stable_id.is_some()
    }

    pub(crate) fn pin_to_id(mut self, pane_id: PaneId) -> Self {
        self.stable_id = Some(pane_id);
        self
    }

    /// Sends literal UTF-8 text bytes to this pane through the daemon.
    ///
    /// The payload is not interpreted as key names, does not expand tmux
    /// formats, and does not receive an implicit trailing newline. Use
    /// [`send_key`](Self::send_key) when a tmux key token such as `Enter`
    /// should be interpreted as a key press.
    pub async fn send_text(&self, text: impl AsRef<str>) -> Result<()> {
        send_text(&self.begin_operation_handle(), text.as_ref()).await
    }

    /// Sends one tmux-compatible key token to this pane through the daemon.
    ///
    /// Tokens keep the daemon's existing `send-keys` semantics: known key
    /// names such as `Enter` are encoded as keys, while ordinary text tokens
    /// are forwarded as their bytes by the server.
    pub async fn send_key(&self, key: impl Into<String>) -> Result<()> {
        send_key(&self.begin_operation_handle(), key.into()).await
    }

    /// Requests an absolute pane size through the daemon.
    ///
    /// Only dimensions that differ from the daemon's current pane details are
    /// sent. The daemon still applies normal `resize-pane` layout rules, so
    /// linked panes, borders, and neighboring panes can constrain the final
    /// geometry. No pane identity is cached by this handle.
    pub async fn resize(&self, size: TerminalSizeSpec) -> Result<()> {
        resize_to_size(&self.begin_operation_handle(), size).await
    }

    /// Sets this pane's UX title label.
    ///
    /// Titles are labels for humans and UI surfaces. They are not technical
    /// identity; use [`Self::id`] and [`Session::pane_by_id`](super::Session::pane_by_id)
    /// for stable addressing.
    pub async fn set_title(&self, title: impl Into<String>) -> Result<()> {
        set_title(&self.begin_operation_handle(), title.into()).await
    }

    /// Returns this pane's current UX title label when the pane still exists.
    pub async fn title(&self) -> Result<Option<String>> {
        get_title(&self.begin_operation_handle()).await
    }

    /// Sets a pane-local option and returns the exact mutation outcome.
    pub async fn set_option(
        &self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<PaneOptionMutation> {
        set_option(&self.begin_operation_handle(), name.into(), value.into()).await
    }

    /// Returns the exact pane-local explicit value for an option.
    pub async fn option(&self, name: impl Into<String>) -> Result<Option<String>> {
        get_option(&self.begin_operation_handle(), name.into()).await
    }

    /// Removes a pane-local explicit option and returns the exact mutation outcome.
    pub async fn unset_option(&self, name: impl Into<String>) -> Result<PaneOptionMutation> {
        unset_option(&self.begin_operation_handle(), name.into()).await
    }

    /// Returns best-effort foreground process state for this pane.
    ///
    /// Foreground changes are detected by a periodic daemon-side probe, so
    /// values are best-effort and may lag the pane by around a second. Use
    /// [`Pane::foreground_state_with_revision`] to order a snapshot against a
    /// [`Pane::state_events`] stream.
    pub async fn foreground_state(&self) -> Result<Option<ForegroundState>> {
        foreground::foreground_state(&self.begin_operation_handle())
            .await
            .map(|state| state.map(|(_, _, foreground)| foreground))
    }

    /// Returns best-effort foreground process state together with the stable
    /// pane id and the pane-state revision the snapshot was taken at, so
    /// callers can order it against a [`Pane::state_events`] stream.
    pub async fn foreground_state_with_revision(
        &self,
    ) -> Result<Option<(PaneId, u64, ForegroundState)>> {
        foreground::foreground_state(&self.begin_operation_handle()).await
    }

    /// Opens a long-poll stream of pane title, option, close, and optional foreground events.
    pub async fn state_events(
        &self,
        options: PaneStateEventsOptions,
    ) -> Result<PaneStateEventStream> {
        state_events::PaneStateEventStream::open(&self.begin_operation_handle(), options).await
    }

    /// Consumes this handle and kills the addressed pane through the daemon.
    ///
    /// A stale handle is treated as an idempotent no-op and returns
    /// [`PaneCloseOutcome::AlreadyClosed`]. Dropping a [`Pane`] handle remains
    /// inert; this consuming method is the SDK operation that explicitly
    /// closes the pane slot and its process.
    pub async fn close(self) -> Result<PaneCloseOutcome> {
        close_pane(self.begin_operation_handle()).await
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
    /// Handles created by [`Session::pane_by_id`](crate::Session::pane_by_id)
    /// keep their stable `%N` identity through daemon-side target resolution;
    /// slot handles use their visible tmux target, including `pane-base-index`.
    pub async fn split(&self, direction: SplitDirection) -> Result<Self> {
        let pane = self.begin_operation_handle();
        let outcome = split_pane(&pane.transport, pane.split_target_text(), direction).await?;
        Ok(Self::new_by_id(
            outcome.target,
            outcome.pane_id,
            pane.endpoint.clone(),
            pane.default_timeout,
            pane.transport,
        ))
    }

    /// Starts building an atomic split that may choose the new pane process.
    ///
    /// Unlike `self.split(direction).await?.spawn(command).await?`, this
    /// builder sends the process specification with the split request, so the
    /// daemon never creates the new pane with an intermediate default shell
    /// that is immediately replaced.
    ///
    /// Stable-id and visible-slot targeting follow the same rules as
    /// [`Self::split`].
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
        respawn_pane(&self.begin_operation_handle(), options).await
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

    pub(crate) fn split_target_text(&self) -> String {
        self.stable_id.map_or_else(
            || self.target.to_proto().to_string(),
            |pane_id| pane_id.to_string(),
        )
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
