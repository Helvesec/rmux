use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use rmux_proto::{OptionName, SessionId, SessionName, TerminalGeometry};

use super::{AttachedSwitchCommitRequest, ClientFlags, RequestHandler};
use crate::pane_io::AttachControl;
use crate::pane_terminals::HandlerState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum SessionDetachOnDestroy {
    Detach,
    MostRecent,
    MostRecentDetached,
    Previous,
    Next,
}

impl SessionDetachOnDestroy {
    pub(in crate::handler) fn capture(state: &HandlerState, session_name: &SessionName) -> Self {
        match state
            .options
            .resolve(Some(session_name), OptionName::DetachOnDestroy)
        {
            Some("off") => Self::MostRecent,
            Some("no-detached") => Self::MostRecentDetached,
            Some("previous") => Self::Previous,
            Some("next") => Self::Next,
            Some("on") | None => Self::Detach,
            Some(_) => Self::Detach,
        }
    }

    pub(in crate::handler) fn capture_all(state: &HandlerState) -> HashMap<SessionId, Self> {
        state
            .sessions
            .iter()
            .map(|(session_name, session)| (session.id(), Self::capture(state, session_name)))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct DestroySwitchCandidate {
    session_name: SessionName,
    session_id: SessionId,
    activity_at: i64,
    created_at: i64,
    attached: bool,
}

#[derive(Debug, Clone)]
struct DestroySwitchPlan {
    attach_pid: u32,
    attach_id: u64,
    size_sequence: u64,
    target: DestroySwitchCandidate,
}

#[derive(Debug, Clone)]
struct DestroyControlSwitchPlan {
    control_pid: u32,
    control_id: u64,
    target_session_id: SessionId,
}

#[derive(Debug, Default)]
struct DestroySwitchPlans {
    attached: Vec<DestroySwitchPlan>,
    control: Vec<DestroyControlSwitchPlan>,
}

#[derive(Debug)]
pub(in crate::handler) struct PreparedAttachedDestroySwitches {
    source_session_id: SessionId,
    plans: Vec<DestroySwitchPlan>,
}

impl RequestHandler {
    pub(in crate::handler) async fn rehome_control_session_identity(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
        detach_on_destroy: SessionDetachOnDestroy,
    ) -> PreparedAttachedDestroySwitches {
        let switch_plans = self
            .destroy_switch_plans(session_name, session_id, detach_on_destroy)
            .await;
        self.apply_control_destroy_switch_plans(session_id, switch_plans.control)
            .await;
        PreparedAttachedDestroySwitches {
            source_session_id: session_id,
            plans: switch_plans.attached,
        }
    }

    pub(in crate::handler) async fn exit_prepared_attached_session_identity(
        &self,
        prepared: PreparedAttachedDestroySwitches,
    ) {
        self.apply_attached_destroy_switch_plans(prepared.source_session_id, prepared.plans)
            .await;
    }

    pub(in crate::handler) async fn exit_attached_session_identity(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
        detach_on_destroy: SessionDetachOnDestroy,
    ) {
        let switch_plans = self
            .destroy_switch_plans(session_name, session_id, detach_on_destroy)
            .await;
        self.apply_control_destroy_switch_plans(session_id, switch_plans.control)
            .await;
        self.apply_attached_destroy_switch_plans(session_id, switch_plans.attached)
            .await;
    }

    async fn apply_attached_destroy_switch_plans(
        &self,
        source_session_id: SessionId,
        plans: Vec<DestroySwitchPlan>,
    ) {
        for plan in plans {
            let Some((terminal_context, client_size, client_pixels, render_stream, client_flags)) =
                self.terminal_context_and_size_for_attached_client_identity(
                    plan.attach_pid,
                    plan.attach_id,
                )
                .await
            else {
                continue;
            };
            let attached_count = self
                .attached_count(&plan.target.session_name)
                .await
                .saturating_add(1);
            if self
                .commit_attached_session_switch(
                    plan.attach_pid,
                    plan.attach_id,
                    AttachedSwitchCommitRequest {
                        expected_current_session_id: Some(source_session_id),
                        session_name: plan.target.session_name.clone(),
                        session_id: plan.target.session_id,
                        target_selection: None,
                        terminal_context,
                        client_geometry: TerminalGeometry {
                            size: client_size,
                            pixels: client_pixels,
                        },
                        client_flags,
                        render_stream,
                        attached_count,
                        client_environment: None,
                    },
                )
                .await
                .is_ok()
            {
                self.emit_client_session_changed(
                    plan.attach_pid,
                    plan.target.session_name,
                    plan.target.session_id,
                )
                .await;
            }
        }
        self.close_attached_session(source_session_id, || AttachControl::Exited)
            .await;
    }

    async fn apply_control_destroy_switch_plans(
        &self,
        source_session_id: SessionId,
        plans: Vec<DestroyControlSwitchPlan>,
    ) {
        for plan in plans {
            if let Some(target_session_name) = self
                .switch_control_session_after_destroy(
                    plan.control_pid,
                    plan.control_id,
                    source_session_id,
                    plan.target_session_id,
                )
                .await
            {
                self.emit_client_session_changed(
                    plan.control_pid,
                    target_session_name,
                    plan.target_session_id,
                )
                .await;
            }
        }
    }

    async fn destroy_switch_plans(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
        detach_on_destroy: SessionDetachOnDestroy,
    ) -> DestroySwitchPlans {
        let control_clients = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .iter()
                .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                .filter_map(|(control_pid, active)| {
                    active
                        .session_id
                        .map(|session_id| (*control_pid, active.id, session_id))
                })
                .collect::<Vec<_>>()
        };
        let attached_control_session_ids = control_clients
            .iter()
            .map(|(_, _, session_id)| *session_id)
            .collect::<HashSet<_>>();
        let state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let attached_session_ids = active_attach
            .by_pid
            .values()
            .filter(|active| !active.closing.load(Ordering::SeqCst))
            .map(|active| active.session_id)
            .chain(attached_control_session_ids)
            .collect::<HashSet<_>>();
        let candidates = state
            .sessions
            .iter()
            .filter(|(candidate_name, _)| *candidate_name != session_name)
            .map(|(candidate_name, session)| DestroySwitchCandidate {
                session_name: candidate_name.clone(),
                session_id: session.id(),
                activity_at: session.activity_at(),
                created_at: session.created_at(),
                attached: attached_session_ids.contains(&session.id()),
            })
            .collect::<Vec<_>>();

        let mut attached = active_attach
            .by_pid
            .iter()
            .filter(|(_, active)| {
                active.session_id == session_id && !active.closing.load(Ordering::SeqCst)
            })
            .filter_map(|(attach_pid, active)| {
                let target = destroy_switch_target(&candidates, session_name, detach_on_destroy)
                    .or_else(|| {
                        if active.flags.contains(ClientFlags::NO_DETACH_ON_DESTROY) {
                            most_recent_destroy_switch_candidate(&candidates)
                        } else {
                            None
                        }
                    });
                target.map(|target| DestroySwitchPlan {
                    attach_pid: *attach_pid,
                    attach_id: active.id,
                    size_sequence: active.size_sequence,
                    target,
                })
            })
            .collect::<Vec<_>>();
        attached.sort_by_key(|plan| (plan.size_sequence, plan.attach_id, plan.attach_pid));

        let control_target = destroy_switch_target(&candidates, session_name, detach_on_destroy);
        let mut control = control_clients
            .into_iter()
            .filter(|(_, _, current_session_id)| *current_session_id == session_id)
            .filter_map(|(control_pid, control_id, _)| {
                control_target
                    .as_ref()
                    .map(|target| DestroyControlSwitchPlan {
                        control_pid,
                        control_id,
                        target_session_id: target.session_id,
                    })
            })
            .collect::<Vec<_>>();
        control.sort_by_key(|plan| (plan.control_id, plan.control_pid));

        DestroySwitchPlans { attached, control }
    }
}

fn destroy_switch_target(
    candidates: &[DestroySwitchCandidate],
    destroyed_session_name: &SessionName,
    policy: SessionDetachOnDestroy,
) -> Option<DestroySwitchCandidate> {
    match policy {
        SessionDetachOnDestroy::Detach => None,
        SessionDetachOnDestroy::MostRecent => most_recent_destroy_switch_candidate(candidates),
        SessionDetachOnDestroy::MostRecentDetached => most_recent_destroy_switch_candidate(
            &candidates
                .iter()
                .filter(|candidate| !candidate.attached)
                .cloned()
                .collect::<Vec<_>>(),
        ),
        SessionDetachOnDestroy::Previous | SessionDetachOnDestroy::Next => {
            adjacent_destroy_switch_candidate(candidates, destroyed_session_name, policy)
        }
    }
}

fn adjacent_destroy_switch_candidate(
    candidates: &[DestroySwitchCandidate],
    destroyed_session_name: &SessionName,
    policy: SessionDetachOnDestroy,
) -> Option<DestroySwitchCandidate> {
    let mut ordered = candidates.to_vec();
    ordered.sort_by(|left, right| {
        left.session_name
            .as_str()
            .cmp(right.session_name.as_str())
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    match policy {
        SessionDetachOnDestroy::Previous => ordered
            .iter()
            .rev()
            .find(|candidate| candidate.session_name.as_str() < destroyed_session_name.as_str())
            .cloned()
            .or_else(|| ordered.last().cloned()),
        SessionDetachOnDestroy::Next => ordered
            .iter()
            .find(|candidate| candidate.session_name.as_str() > destroyed_session_name.as_str())
            .cloned()
            .or_else(|| ordered.first().cloned()),
        _ => unreachable!("adjacent destroy policy was validated by caller"),
    }
}

fn most_recent_destroy_switch_candidate(
    candidates: &[DestroySwitchCandidate],
) -> Option<DestroySwitchCandidate> {
    candidates
        .iter()
        .max_by(|left, right| {
            left.activity_at
                .cmp(&right.activity_at)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.session_id.cmp(&right.session_id))
                .then_with(|| right.session_name.as_str().cmp(left.session_name.as_str()))
        })
        .cloned()
}
