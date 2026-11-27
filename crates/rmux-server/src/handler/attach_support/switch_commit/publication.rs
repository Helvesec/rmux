//! Post-commit publication of attached `switch-client` successes and failures.

use rmux_core::LifecycleEvent;
use rmux_proto::{RmuxError, SessionId, SessionName};
use tracing::warn;

use super::{AttachedSwitchCommitFailure, AttachedSwitchCommitOutcome, RequestHandler};

impl RequestHandler {
    pub(in crate::handler) async fn finish_attached_switch_failure(
        &self,
        failure: AttachedSwitchCommitFailure,
    ) -> RmuxError {
        if let Some(focus_effects) = failure.focus_effects {
            if let Err(error) = focus_effects.execute(self).await {
                warn!("failed to publish failed-switch detach focus transition: {error}");
            }
        }
        if let Some(detached_client) = failure.detached_client {
            self.emit_without_attached_refresh(LifecycleEvent::ClientDetached {
                session_name: detached_client.session_name,
                client_name: Some(detached_client.client_name),
            })
            .await;
        }
        failure.error
    }

    /// Publishes what tmux publishes once an attached `switch-client` has
    /// committed.
    ///
    /// tmux 3.7b, measured 2026-07-25 (source session with a 101x41 PTY client
    /// and a 60x20 control client, `window-size largest`): after the PTY client
    /// switches away, the control client that stayed on the source session
    /// receives
    ///     %client-session-changed /dev/ttys007 $1 target
    ///     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    /// in that order, and the source window really does shrink to the surviving
    /// client's geometry. Targeted selection transitions already prepared by
    /// the commit are published first (pane, then window), followed by this
    /// client move. Only then is the source session reconciled, preserving the
    /// measured client-before-layout order.
    pub(in crate::handler) async fn finish_attached_session_switch(
        &self,
        outcome: AttachedSwitchCommitOutcome,
        session_name: SessionName,
        session_id: SessionId,
    ) {
        let AttachedSwitchCommitOutcome {
            previous_session_id,
            client_name,
            ..
        } = outcome;
        self.emit_client_session_changed(client_name, session_name, session_id)
            .await;
        if previous_session_id != session_id {
            let _ = self
                .reconcile_attached_session_identity_size_and_emit(previous_session_id)
                .await;
            self.destroy_unattached_session_identity(previous_session_id)
                .await;
        }
    }
}
