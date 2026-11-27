use rmux_core::LifecycleEvent;
use rmux_proto::SessionId;

use super::{
    close_control_with_exit, ControlClientDetachOutcome, ControlClientIdentity,
    ControlClientsDetachOutcome,
};
use crate::client_names::control_client_name;
use crate::handler::{
    prepare_unsequenced_lifecycle_event, sequence_prepared_lifecycle_event, RequestHandler,
};
use crate::handler_support::attached_client_required;

impl RequestHandler {
    pub(in crate::handler) async fn detach_control_clients_for_session_identity(
        &self,
        session_id: SessionId,
        control_clients: Vec<ControlClientIdentity>,
        reason: Option<String>,
    ) -> ControlClientsDetachOutcome {
        let mut state = self.state.lock().await;
        let Some(session_name) = state
            .sessions
            .session_by_id(session_id)
            .map(|session| session.name().clone())
        else {
            return ControlClientsDetachOutcome {
                lifecycle_events: Vec::new(),
            };
        };
        let mut active_control = self.active_control.lock().await;
        let lifecycle_events = control_clients
            .into_iter()
            .filter_map(|identity| {
                let requester_pid = identity.requester_pid();
                let matches_snapshot =
                    active_control
                        .by_pid
                        .get(&requester_pid)
                        .is_some_and(|active| {
                            active.id == identity.control_id()
                                && active.session_id == Some(session_id)
                        });
                if !matches_snapshot {
                    return None;
                }
                let active = active_control.by_pid.remove(&requester_pid)?;
                let prepare_detached = !active.client_detached_prepared;
                close_control_with_exit(&active, reason.clone());
                prepare_detached.then(|| {
                    prepare_unsequenced_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::ClientDetached {
                            session_name: session_name.clone(),
                            client_name: Some(control_client_name(requester_pid)),
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        let _publication = state.track_unordered_lifecycle_publication();

        drop(active_control);
        drop(state);
        // Explicit detach recalculates shared windows before the departure
        // notifications. Removing these registrations also means finish_control
        // cannot perform the resize later on behalf of this batch.
        let _ = self
            .reconcile_attached_session_identity_size_and_emit(session_id)
            .await;
        let mut state = self.state.lock().await;
        let lifecycle_events = lifecycle_events
            .into_iter()
            .map(|event| {
                let mut event = sequence_prepared_lifecycle_event(&mut state, event);
                event.control_session_identity = Some(session_id);
                event
            })
            .collect();
        ControlClientsDetachOutcome { lifecycle_events }
    }

    pub(in crate::handler) async fn exit_control_client_for_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        reason: Option<String>,
    ) -> Result<ControlClientDetachOutcome, rmux_proto::RmuxError> {
        self.exit_control_client_with_expected_id(requester_pid, expected_control_id, reason, None)
            .await
    }

    pub(in crate::handler) async fn exit_control_client_for_identity_from_mode_tree(
        &self,
        requester: super::super::mode_tree_support::ModeTreeActionIdentity,
        requester_pid: u32,
        expected_control_id: u64,
        reason: Option<String>,
    ) -> Result<ControlClientDetachOutcome, rmux_proto::RmuxError> {
        self.exit_control_client_with_expected_id(
            requester_pid,
            expected_control_id,
            reason,
            Some(requester),
        )
        .await
    }

    async fn exit_control_client_with_expected_id(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        reason: Option<String>,
        mode_tree_requester: Option<super::super::mode_tree_support::ModeTreeActionIdentity>,
    ) -> Result<ControlClientDetachOutcome, rmux_proto::RmuxError> {
        let mut state = self.state.lock().await;
        let mode_tree_guard = if let Some(identity) = mode_tree_requester {
            let active_attach = self.active_attach.lock().await;
            if !identity.matches_active(&state, &active_attach) {
                return Err(attached_client_required("detach-client"));
            }
            Some(active_attach)
        } else {
            None
        };
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
            return Err(attached_client_required("detach-client"));
        };
        if active.id != expected_control_id {
            return Err(attached_client_required("detach-client"));
        }
        let session_name = active.session_name.clone();
        let session_id = active.session_id;
        let prepare_detached = session_name.is_some() && !active.client_detached_prepared;
        active.client_detached_prepared |= prepare_detached;
        close_control_with_exit(active, reason);
        let lifecycle_event = session_name
            .filter(|_| prepare_detached)
            .map(|session_name| {
                prepare_unsequenced_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::ClientDetached {
                        session_name,
                        client_name: Some(control_client_name(requester_pid)),
                    },
                )
            });
        let _publication = state.track_unordered_lifecycle_publication();
        drop(active_control);
        drop(mode_tree_guard);
        drop(state);
        if let Some(session_id) = session_id {
            // Marking the client closing removes its size vote. Publish that
            // change before callers publish the prepared client-detached event.
            let _ = self
                .reconcile_attached_session_identity_size_and_emit(session_id)
                .await;
        }
        // Freeze the departing identity and hook context under the original
        // lock, but reserve its publication turn only after the resize events.
        let mut state = self.state.lock().await;
        let lifecycle_event = lifecycle_event.map(|event| {
            let mut event = sequence_prepared_lifecycle_event(&mut state, event);
            event.control_session_identity = session_id;
            event
        });
        Ok(ControlClientDetachOutcome { lifecycle_event })
    }
}
