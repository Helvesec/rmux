//! Raw stream initialization: reuse a cached rebase, wait for one, or capture a new boundary.

use std::sync::{Arc, Mutex as StdMutex, Weak};

use rmux_proto::{PaneRawRebaseReason, Response};

use crate::pane_io::{PaneBoundary, PaneOutputReceiver};

use super::super::super::subscription_support::{OutputSubscriptionState, RawInitializationRoute};
use super::super::{
    reserved_stream_key_if_owned, CachedRawRebase, CapturedPaneBoundary, PaneStreamSource,
    RequestHandler,
};

pub(in crate::handler::pane_stream_support) enum RawInitializationOutcome {
    Complete(Response),
    Capture {
        source: PaneStreamSource,
        guard: RawInitializationGuard,
    },
}

pub(in crate::handler::pane_stream_support) struct RawInitializationGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    token: u64,
}

pub(in crate::handler::pane_stream_support) struct RawSubscriptionStart {
    pub(super) receiver: PaneOutputReceiver,
    pub(super) boundary: PaneBoundary,
    pub(super) rebase: rmux_proto::PaneRawRebase,
    pub(super) include_snapshot: bool,
    pub(super) cached_rebase: Option<Arc<CachedRawRebase>>,
}

impl RawSubscriptionStart {
    pub(in crate::handler::pane_stream_support) fn captured(
        captured: CapturedPaneBoundary,
        rebase: rmux_proto::PaneRawRebase,
        include_snapshot: bool,
    ) -> Self {
        Self {
            receiver: captured.receiver,
            boundary: captured.boundary,
            rebase,
            include_snapshot,
            cached_rebase: None,
        }
    }

    fn cached(
        receiver: PaneOutputReceiver,
        cached_rebase: Arc<CachedRawRebase>,
        rebase: rmux_proto::PaneRawRebase,
        include_snapshot: bool,
    ) -> Self {
        Self {
            receiver,
            boundary: cached_rebase.boundary,
            rebase,
            include_snapshot,
            cached_rebase: Some(cached_rebase),
        }
    }
}

impl RawInitializationGuard {
    fn new(subscriptions: &Arc<StdMutex<OutputSubscriptionState>>, token: u64) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            token,
        }
    }
}

impl Drop for RawInitializationGuard {
    fn drop(&mut self) {
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish_raw_initialization(self.token);
    }
}

impl RequestHandler {
    pub(in crate::handler::pane_stream_support) async fn prepare_initial_raw_subscription(
        &self,
        connection_id: u64,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        mut source: PaneStreamSource,
        mut route: RawInitializationRoute,
        include_snapshot: bool,
    ) -> RawInitializationOutcome {
        loop {
            match route {
                RawInitializationRoute::Ready(cached) => {
                    if let Some(receiver) = source.output.subscribe_at_boundary(cached.boundary) {
                        let rebase = Self::initial_rebase_from_cache(&cached, include_snapshot);
                        return RawInitializationOutcome::Complete(self.finish_raw_subscription(
                            connection_id,
                            subscription_id,
                            source,
                            RawSubscriptionStart::cached(
                                receiver,
                                cached,
                                rebase,
                                include_snapshot,
                            ),
                        ));
                    }
                    let mut subscriptions = self
                        .subscriptions
                        .lock()
                        .expect("subscription registry mutex must not be poisoned");
                    let Some(current_key) = reserved_stream_key_if_owned(
                        &subscriptions,
                        connection_id,
                        subscription_id,
                        source.key.pane_id(),
                    ) else {
                        return RawInitializationOutcome::Complete(
                            super::super::reserved_stream_lost_response(),
                        );
                    };
                    source.rekey(current_key);
                    subscriptions.discard_raw_rebase_if_current(&source.key, &cached);
                    route = subscriptions.raw_initialization_route(&source.key, include_snapshot);
                }
                RawInitializationRoute::Initialize { token } => {
                    return RawInitializationOutcome::Capture {
                        source,
                        guard: RawInitializationGuard::new(&self.subscriptions, token),
                    };
                }
                RawInitializationRoute::Wait(mut completion) => {
                    let _ = completion.changed().await;
                    let mut subscriptions = self
                        .subscriptions
                        .lock()
                        .expect("subscription registry mutex must not be poisoned");
                    let Some(current_key) = reserved_stream_key_if_owned(
                        &subscriptions,
                        connection_id,
                        subscription_id,
                        source.key.pane_id(),
                    ) else {
                        return RawInitializationOutcome::Complete(
                            super::super::reserved_stream_lost_response(),
                        );
                    };
                    source.rekey(current_key);
                    route = subscriptions.raw_initialization_route(&source.key, include_snapshot);
                }
            }
        }
    }

    fn initial_rebase_from_cache(
        cached: &CachedRawRebase,
        include_snapshot: bool,
    ) -> rmux_proto::PaneRawRebase {
        let rebase = &cached.rebase;
        rmux_proto::PaneRawRebase {
            epoch: 1,
            generation: rebase.generation,
            invalidation_revision: rebase.invalidation_revision,
            next_sequence: rebase.next_sequence,
            cols: rebase.cols,
            rows: rebase.rows,
            keyframe: rebase.keyframe.clone(),
            alternate: rebase.alternate,
            coverage: rebase.coverage,
            snapshot: if include_snapshot {
                rebase.snapshot.clone()
            } else {
                None
            },
            reason: PaneRawRebaseReason::Initial,
        }
    }
}
