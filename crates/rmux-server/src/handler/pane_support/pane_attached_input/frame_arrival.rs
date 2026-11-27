//! What a live frame of a client found up when it arrived.
//!
//! tmux classifies every key of a read before any command those keys run, so a display-panes that
//! one of them shows (`display-panes -b` does not wait) does not take the rest of that read: the
//! rest goes to the key tables and the pane. RMUX handles a frame's keys one after another, so it
//! records the display-panes up when the frame arrived and lets only that one take the frame's keys.

use std::future::Future;
use std::sync::atomic::Ordering;

use super::{ActiveAttachIdentity, RequestHandler};

tokio::task_local! {
    /// The display-panes of the client, by instance, when the live frame being handled arrived.
    static DISPLAY_PANES_AT_ARRIVAL: Option<u64>;
}

impl RequestHandler {
    /// Handles `frame`, a live frame of the client, with what it found up when it arrived.
    pub(super) async fn handle_arrived_frame<F: Future>(
        &self,
        identity: ActiveAttachIdentity,
        frame: F,
    ) -> F::Output {
        let display_panes = self.display_panes_instance(identity).await;
        DISPLAY_PANES_AT_ARRIVAL.scope(display_panes, frame).await
    }

    /// Whether the client's display-panes takes this input: within a live frame, only if it was up
    /// when the frame arrived; outside one - the escape time's flush - as it is.
    pub(super) async fn display_panes_takes_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> bool {
        let Some(shown) = self.display_panes_instance(identity).await else {
            return false;
        };
        DISPLAY_PANES_AT_ARRIVAL
            .try_with(|at_arrival| *at_arrival == Some(shown))
            .unwrap_or(true)
    }

    async fn display_panes_instance(&self, identity: ActiveAttachIdentity) -> Option<u64> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active) && !active.closing.load(Ordering::SeqCst)
            })
            .and_then(|active| active.display_panes.as_ref())
            .map(|display_panes| display_panes.id)
    }
}
