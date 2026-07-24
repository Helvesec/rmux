use std::collections::VecDeque;
use std::time::Duration;

use rmux_proto::{
    PaneOutputSubscriptionId, PaneStreamCursorRequest, PaneStreamEvent as ProtoEvent,
    PaneStreamLifecycleEvent as ProtoLifecycle, PaneStreamMode, PaneTargetRef, Request, Response,
    SubscribePaneStreamRequest, UnsubscribePaneStreamRequest,
};

use crate::handles::session::unexpected_response;
use crate::transport::{DropGuard, TransportClient};
use crate::{Result, RmuxError};

const PANE_STREAM_BATCH_SIZE: u16 = 256;
const POLL_INITIAL_DELAY: Duration = Duration::from_millis(2);
const POLL_MAX_DELAY: Duration = Duration::from_millis(50);

/// A pane process transition that does not remove the logical pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneStreamLifecycleEvent {
    /// The current child process reached EOF and exited.
    ///
    /// A kept pane can later be respawned without opening a new stream.
    ProcessExited {
        /// Output sequence occupied by the EOF marker when it remained
        /// observable. `None` means a rebase or retention gap already owns
        /// sequence continuity for this lifecycle observation.
        output_sequence: Option<u64>,
    },
}

/// Why a recoverable pane stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PaneStreamEndReason {
    /// The logical pane was removed.
    PaneRemoved,
    /// Observation permission was revoked.
    AccessRevoked,
    /// The bounded consumer could not keep up.
    SlowConsumer,
    /// The daemon could not materialize the requested projection.
    ProjectionFailed,
    /// The idle stream lease expired.
    SubscriptionExpired,
    /// The detached transport was lost.
    TransportLost,
}

pub(super) struct MappedEvent<T> {
    pub(super) value: T,
    pub(super) terminal: bool,
}

impl<T> MappedEvent<T> {
    pub(super) const fn live(value: T) -> Self {
        Self {
            value,
            terminal: false,
        }
    }

    pub(super) const fn terminal(value: T) -> Self {
        Self {
            value,
            terminal: true,
        }
    }
}

pub(super) struct RecoverablePaneStream<T> {
    inner: PaneStreamSubscription,
    pending: VecDeque<MappedEvent<T>>,
    poll_delay: Duration,
    cursor_request: Option<tokio::task::JoinHandle<Result<Response>>>,
    map_event: fn(ProtoEvent) -> Result<MappedEvent<T>>,
}

struct PaneStreamSubscription {
    transport: TransportClient,
    subscription_id: PaneOutputSubscriptionId,
    _drop_guard: DropGuard,
    closed: bool,
}

impl<T> RecoverablePaneStream<T> {
    pub(super) async fn open(
        transport: TransportClient,
        target: PaneTargetRef,
        mode: PaneStreamMode,
        include_snapshot: bool,
        map_event: fn(ProtoEvent) -> Result<MappedEvent<T>>,
    ) -> Result<Self> {
        let response = transport
            .request(Request::SubscribePaneStream(SubscribePaneStreamRequest {
                target,
                mode,
                include_snapshot,
            }))
            .await?;
        let response = match response {
            Response::SubscribePaneStream(response) => response,
            response => return Err(unexpected_response("subscribe-pane-stream", response)),
        };
        let subscription_id = response.subscription_id;
        let initial = map_event(response.event)?;
        let transport = transport.reusable();
        let drop_guard = DropGuard::best_effort(
            transport.clone(),
            Request::UnsubscribePaneStream(UnsubscribePaneStreamRequest { subscription_id }),
        );
        Ok(Self {
            inner: PaneStreamSubscription {
                transport,
                subscription_id,
                _drop_guard: drop_guard,
                closed: false,
            },
            pending: VecDeque::from([initial]),
            poll_delay: POLL_INITIAL_DELAY,
            cursor_request: None,
            map_event,
        })
    }

    pub(super) async fn next(&mut self) -> Result<Option<T>> {
        if let Some(event) = self.pop_pending() {
            return Ok(Some(event));
        }
        if self.inner.closed {
            return Ok(None);
        }

        loop {
            self.refill_once().await?;
            if let Some(event) = self.pop_pending() {
                self.poll_delay = POLL_INITIAL_DELAY;
                return Ok(Some(event));
            }
            let delay = self.poll_delay;
            self.poll_delay = (self.poll_delay * 2).min(POLL_MAX_DELAY);
            tokio::time::sleep(delay).await;
        }
    }

    pub(super) async fn poll_once(&mut self) -> Result<Vec<T>> {
        if !self.inner.closed && self.pending.is_empty() {
            self.refill_once().await?;
        }
        let pending = self.pending.drain(..).collect::<Vec<_>>();
        let terminal = pending.iter().any(|event| event.terminal);
        if terminal {
            self.inner.closed = true;
        }
        Ok(pending.into_iter().map(|event| event.value).collect())
    }

    async fn refill_once(&mut self) -> Result<()> {
        if self.cursor_request.is_none() {
            let transport = self.inner.transport.clone();
            let request = Request::PaneStreamCursor(PaneStreamCursorRequest {
                subscription_id: self.inner.subscription_id,
                max_events: Some(PANE_STREAM_BATCH_SIZE),
            });
            self.cursor_request = Some(tokio::spawn(
                async move { transport.request(request).await },
            ));
        }
        let response = self
            .cursor_request
            .as_mut()
            .expect("pane stream cursor request exists")
            .await;
        self.cursor_request = None;
        let response = response.map_err(|error| {
            RmuxError::transport(
                "join pane-stream cursor poll",
                std::io::Error::other(error.to_string()),
            )
        })?;
        let response = match response {
            Ok(response) => response,
            Err(RmuxError::Transport { .. }) => {
                self.pending.push_back((self.map_event)(ProtoEvent::End(
                    rmux_proto::PaneStreamEndReason::TransportLost,
                ))?);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let response = match response {
            Response::PaneStreamCursor(response) => response,
            response => return Err(unexpected_response("pane-stream-cursor", response)),
        };
        self.validate_subscription(response.subscription_id)?;
        let mut mapped = VecDeque::with_capacity(response.events.len());
        for event in response.events {
            if mapped
                .back()
                .is_some_and(|event: &MappedEvent<T>| event.terminal)
            {
                return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "rmux daemon sent pane-stream events after a terminal end".to_owned(),
                )));
            }
            mapped.push_back((self.map_event)(event)?);
        }
        self.pending.append(&mut mapped);
        Ok(())
    }

    fn pop_pending(&mut self) -> Option<T> {
        let event = self.pending.pop_front()?;
        if event.terminal {
            self.inner.closed = true;
            self.pending.clear();
        }
        Some(event.value)
    }

    fn validate_subscription(&self, response_id: PaneOutputSubscriptionId) -> Result<()> {
        if response_id == self.inner.subscription_id {
            return Ok(());
        }
        Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon sent subscription id {} in `pane-stream-cursor` response for subscription {}",
            response_id.as_u64(),
            self.inner.subscription_id.as_u64()
        ))))
    }
}

pub(super) const fn lifecycle_from_proto(lifecycle: ProtoLifecycle) -> PaneStreamLifecycleEvent {
    match lifecycle {
        ProtoLifecycle::ProcessExited { output_sequence } => {
            PaneStreamLifecycleEvent::ProcessExited { output_sequence }
        }
    }
}

pub(super) const fn end_from_proto(reason: rmux_proto::PaneStreamEndReason) -> PaneStreamEndReason {
    match reason {
        rmux_proto::PaneStreamEndReason::PaneRemoved => PaneStreamEndReason::PaneRemoved,
        rmux_proto::PaneStreamEndReason::AccessRevoked => PaneStreamEndReason::AccessRevoked,
        rmux_proto::PaneStreamEndReason::SlowConsumer => PaneStreamEndReason::SlowConsumer,
        rmux_proto::PaneStreamEndReason::ProjectionFailed => PaneStreamEndReason::ProjectionFailed,
        rmux_proto::PaneStreamEndReason::SubscriptionExpired => {
            PaneStreamEndReason::SubscriptionExpired
        }
        rmux_proto::PaneStreamEndReason::TransportLost => PaneStreamEndReason::TransportLost,
    }
}
