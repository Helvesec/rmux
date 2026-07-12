use std::collections::VecDeque;

use crate::handles::connect_transport_to_endpoint;
use crate::handles::session::unexpected_response;
use crate::transport::TransportClient;
use crate::{Pane, PaneId, Result};
use rmux_proto::{
    PaneOptionEntry as ProtoPaneOptionEntry, PaneStateCursorRequest, PaneStateEventDto,
    PaneStateSnapshot, PaneStateSubscriptionId, Request, Response, SubscribePaneStateRequest,
    CAPABILITY_SDK_PANE_BY_ID, CAPABILITY_SDK_PANE_FOREGROUND, CAPABILITY_SDK_PANE_STATE_EVENTS,
};

use super::foreground::ForegroundState;
use super::target::stale_slot_error;

pub use rmux_proto::PaneStateClosedReason;

const PANE_STATE_BATCH_SIZE: u16 = 256;

/// Options used when opening a pane-state event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneStateEventsOptions {
    /// Include title snapshots and title-change events.
    pub include_title: bool,
    /// Include pane-local option snapshots and option mutation events.
    pub include_options: bool,
    /// Include best-effort foreground snapshots and foreground-change events.
    pub include_foreground: bool,
}

impl Default for PaneStateEventsOptions {
    fn default() -> Self {
        Self {
            include_title: true,
            include_options: true,
            include_foreground: false,
        }
    }
}

/// One explicit pane option in a pane-state snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneStateOption {
    /// Canonical option name.
    pub name: String,
    /// Exact explicit option value.
    pub value: String,
}

/// One item from a pane-state event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaneStateEvent {
    /// Initial or rebased state.
    Snapshot {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Current title when requested.
        title: Option<String>,
        /// Current explicit pane options when requested.
        options: Vec<PaneStateOption>,
        /// Best-effort foreground state when requested.
        foreground: Option<ForegroundState>,
    },
    /// The pane title changed.
    TitleChanged {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Previous title.
        old_title: String,
        /// New title.
        new_title: String,
    },
    /// A pane-local option was set or replaced.
    OptionSet {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Canonical option name.
        name: String,
        /// Previous explicit value.
        old_value: Option<String>,
        /// New explicit value.
        new_value: String,
    },
    /// A pane-local option was unset.
    OptionUnset {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Canonical option name.
        name: String,
        /// Previous explicit value.
        old_value: Option<String>,
    },
    /// Best-effort foreground process state changed.
    ForegroundChanged {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Previous foreground state.
        old_state: ForegroundState,
        /// New foreground state.
        new_state: ForegroundState,
    },
    /// The local cursor fell behind the daemon's bounded journal.
    Lagged {
        /// Cursor revision that was too old.
        missed_from_revision: u64,
        /// Oldest retained revision after the gap.
        resume_revision: u64,
    },
    /// The pane reached a terminal state for this stream.
    Closed {
        /// Global pane-state journal revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Terminal close reason.
        reason: PaneStateClosedReason,
    },
}

/// Opaque long-poll stream for pane title/option/foreground/close events.
pub struct PaneStateEventStream {
    cursor_transport: TransportClient,
    subscription_id: PaneStateSubscriptionId,
    pane_id: PaneId,
    next_revision: u64,
    pending: VecDeque<PaneStateEvent>,
    closed: bool,
    cursor_request: Option<tokio::task::JoinHandle<Result<Response>>>,
}

impl PaneStateEventStream {
    pub(super) async fn open(pane: &Pane, options: PaneStateEventsOptions) -> Result<Self> {
        let Some(target) = pane.resolved_proto_target_ref().await? else {
            return Err(stale_slot_error(pane.target()));
        };
        let timeout = crate::wait::resolved_wait_timeout(pane.configured_default_timeout());
        let cursor_transport = connect_transport_to_endpoint(pane.endpoint(), timeout).await?;
        Self::open_with_cursor_transport_and_target(pane, options, cursor_transport, target).await
    }

    #[cfg(test)]
    pub(super) async fn open_with_cursor_transport(
        pane: &Pane,
        options: PaneStateEventsOptions,
        cursor_transport: TransportClient,
    ) -> Result<Self> {
        Self::open_with_cursor_transport_and_target(
            pane,
            options,
            cursor_transport,
            pane.proto_target_ref(),
        )
        .await
    }

    async fn open_with_cursor_transport_and_target(
        pane: &Pane,
        options: PaneStateEventsOptions,
        cursor_transport: TransportClient,
        target: rmux_proto::PaneTargetRef,
    ) -> Result<Self> {
        let mut capabilities = vec![CAPABILITY_SDK_PANE_STATE_EVENTS];
        if options.include_foreground {
            capabilities.push(CAPABILITY_SDK_PANE_FOREGROUND);
        }
        if matches!(target, rmux_proto::PaneTargetRef::Id { .. }) {
            capabilities.push(CAPABILITY_SDK_PANE_BY_ID);
        }
        crate::capabilities::require(pane.transport(), &capabilities).await?;
        crate::capabilities::require_with_handshake(
            &cursor_transport,
            &capabilities,
            &capabilities,
        )
        .await?;

        let response = cursor_transport
            .request(Request::SubscribePaneState(SubscribePaneStateRequest {
                target,
                include_title: options.include_title,
                include_options: options.include_options,
                include_foreground: options.include_foreground,
            }))
            .await?;

        let response = match response {
            Response::SubscribePaneState(response) => *response,
            response => return Err(unexpected_response("subscribe-pane-state", response)),
        };
        let mut pending = VecDeque::new();
        pending.push_back(snapshot_event(response.pane_id, response.snapshot));

        Ok(Self {
            cursor_transport,
            subscription_id: response.subscription_id,
            pane_id: response.pane_id,
            next_revision: 0,
            pending,
            closed: false,
            cursor_request: None,
        })
    }

    /// Returns the next pane-state event, blocking on the daemon when needed.
    pub async fn next(&mut self) -> Result<Option<PaneStateEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                self.observe_event_cursor(&event);
                return Ok(Some(event));
            }
            if self.closed {
                return Ok(None);
            }

            if self.cursor_request.is_none() {
                let cursor_transport = self.cursor_transport.clone();
                let request = Request::PaneStateCursor(PaneStateCursorRequest {
                    subscription_id: self.subscription_id,
                    after_revision: self.next_revision,
                    wait: true,
                    max_events: Some(PANE_STATE_BATCH_SIZE),
                });
                self.cursor_request = Some(tokio::spawn(async move {
                    cursor_transport.request(request).await
                }));
            }
            let response = self
                .cursor_request
                .as_mut()
                .expect("cursor request exists")
                .await;
            self.cursor_request = None;
            let response = response.map_err(|error| {
                crate::RmuxError::transport(
                    "join pane-state cursor poll",
                    std::io::Error::other(error.to_string()),
                )
            })?;
            let response = response?;

            match response {
                Response::PaneStateCursor(response) => {
                    self.next_revision = response.next_revision;
                    self.pending
                        .extend(response.events.into_iter().map(PaneStateEvent::from));
                }
                Response::PaneStateLag(response) => {
                    self.pending.push_back(PaneStateEvent::Lagged {
                        missed_from_revision: response.missed_from_revision,
                        resume_revision: response.resume_revision,
                    });
                    self.next_revision = response.snapshot.revision;
                    self.pending
                        .push_back(snapshot_event(self.pane_id, response.snapshot));
                }
                response => return Err(unexpected_response("pane-state-cursor", response)),
            }
        }
    }

    fn observe_event_cursor(&mut self, event: &PaneStateEvent) {
        match event {
            PaneStateEvent::Snapshot { revision, .. } => {
                self.next_revision = self.next_revision.max(*revision);
            }
            PaneStateEvent::TitleChanged { revision, .. }
            | PaneStateEvent::OptionSet { revision, .. }
            | PaneStateEvent::OptionUnset { revision, .. }
            | PaneStateEvent::ForegroundChanged { revision, .. }
            | PaneStateEvent::Closed { revision, .. } => {
                self.next_revision = self.next_revision.max(*revision);
            }
            PaneStateEvent::Lagged { .. } => {}
        }
        if matches!(event, PaneStateEvent::Closed { .. }) {
            self.closed = true;
            self.pending.clear();
        }
    }
}

impl Drop for PaneStateEventStream {
    fn drop(&mut self) {
        if let Some(request) = self.cursor_request.take() {
            request.abort();
        }
        // This transport is dedicated to this stream. Closing it is both
        // cancellation-safe and sufficient for server-side subscription
        // cleanup, unlike queueing an unsubscribe behind a pending long poll.
        self.cursor_transport.abort();
    }
}

impl std::fmt::Debug for PaneStateEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneStateEventStream")
            .field("subscription_id", &self.subscription_id)
            .field("pane_id", &self.pane_id)
            .field("next_revision", &self.next_revision)
            .field("pending_events", &self.pending.len())
            .field("closed", &self.closed)
            .field("cursor_request_in_flight", &self.cursor_request.is_some())
            .finish_non_exhaustive()
    }
}

impl From<PaneStateEventDto> for PaneStateEvent {
    fn from(value: PaneStateEventDto) -> Self {
        match value {
            PaneStateEventDto::TitleChanged {
                revision,
                pane_id,
                old_title,
                new_title,
            } => Self::TitleChanged {
                revision,
                pane_id,
                old_title,
                new_title,
            },
            PaneStateEventDto::OptionSet {
                revision,
                pane_id,
                name,
                old_value,
                new_value,
            } => Self::OptionSet {
                revision,
                pane_id,
                name,
                old_value,
                new_value,
            },
            PaneStateEventDto::OptionUnset {
                revision,
                pane_id,
                name,
                old_value,
            } => Self::OptionUnset {
                revision,
                pane_id,
                name,
                old_value,
            },
            PaneStateEventDto::ForegroundChanged {
                revision,
                pane_id,
                old_state,
                new_state,
            } => Self::ForegroundChanged {
                revision,
                pane_id,
                old_state: ForegroundState::from(old_state),
                new_state: ForegroundState::from(new_state),
            },
            PaneStateEventDto::Closed {
                revision,
                pane_id,
                reason,
            } => Self::Closed {
                revision,
                pane_id,
                reason,
            },
            _ => unreachable!("unknown pane-state event variant from this rmux-proto version"),
        }
    }
}

fn snapshot_event(pane_id: PaneId, snapshot: PaneStateSnapshot) -> PaneStateEvent {
    PaneStateEvent::Snapshot {
        revision: snapshot.revision,
        pane_id,
        title: snapshot.title,
        options: snapshot
            .options
            .into_iter()
            .map(PaneStateOption::from)
            .collect(),
        foreground: snapshot.foreground.map(ForegroundState::from),
    }
}

impl From<ProtoPaneOptionEntry> for PaneStateOption {
    fn from(value: ProtoPaneOptionEntry) -> Self {
        Self {
            name: value.name,
            value: value.value,
        }
    }
}
