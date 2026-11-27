//! Output subscriptions of an exited pane: staging its sources, draining and idle cleanup.

use std::time::{Duration, Instant};

use rmux_core::events::PaneOutputSubscriptionKey;
use rmux_proto::PaneStreamEndReason;

use super::super::RequestHandler;

const EXITED_PANE_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(25);
const EXITED_PANE_DRAIN_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

impl RequestHandler {
    /// Makes an exited pane's immutable capture handles visible while the
    /// caller still holds the state lock that removed the pane. Cursor paths
    /// resolve live state first and this staged source second, so they cannot
    /// observe a gap between those two ownership locations.
    pub(in crate::handler) fn stage_exited_pane_stream_source(
        &self,
        pane: PaneOutputSubscriptionKey,
        source: super::super::pane_stream_support::PaneStreamSource,
    ) {
        self.subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned")
            .stage_pane_drain_source(pane, source);
    }

    pub(in crate::handler) async fn drain_exited_pane_output_subscriptions(
        &self,
        pane: PaneOutputSubscriptionKey,
        source: Option<super::super::pane_stream_support::PaneStreamSource>,
    ) {
        let should_watch = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.mark_pane_streams_ending(&pane, PaneStreamEndReason::PaneRemoved);
            subscriptions.begin_pane_drain(pane.clone(), source, Instant::now())
        };
        if should_watch {
            self.watch_exited_pane_drain(pane);
        }
    }

    pub(super) fn watch_exited_pane_drain(&self, pane: PaneOutputSubscriptionKey) {
        let handler = self.downgrade();
        tokio::spawn(async move {
            loop {
                let Some(handler) = handler.upgrade() else {
                    return;
                };
                if handler.pane_drain_finished(&pane).await {
                    return;
                }
                if handler
                    .cleanup_drained_pane_output_subscriptions_if_idle(&pane)
                    .await
                {
                    return;
                }
                tokio::time::sleep(EXITED_PANE_DRAIN_POLL_INTERVAL).await;
            }
        });
    }

    async fn pane_drain_finished(&self, pane: &PaneOutputSubscriptionKey) -> bool {
        let subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        !subscriptions.pane_is_draining(pane)
    }

    async fn cleanup_drained_pane_output_subscriptions_if_idle(
        &self,
        pane: &PaneOutputSubscriptionKey,
    ) -> bool {
        let expired = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned");
            subscriptions.expire_pane_drain_if_idle(
                pane,
                Instant::now(),
                EXITED_PANE_DRAIN_IDLE_TIMEOUT,
            )
        };
        if expired {
            let _ = self.request_shutdown_if_pending();
        }
        expired
    }
}
