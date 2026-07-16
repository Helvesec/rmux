use std::collections::HashSet;
use std::sync::atomic::Ordering;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    OptionName, RmuxError, SessionId, SessionName, TerminalSize, WindowId, WindowTarget,
};

use crate::pane_io::AttachControl;

use super::super::RequestHandler;

#[path = "resize_policy/identity.rs"]
mod identity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum AttachedWindowSizePolicy {
    Latest,
    Largest,
    Smallest,
    Manual,
}

#[derive(Debug, Clone, Copy)]
struct AttachedSizeCandidate {
    size: TerminalSize,
    sequence: u64,
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct AttachedSizeSelection {
    pub(in crate::handler) selected_size: Option<TerminalSize>,
    pub(in crate::handler) session_id: SessionId,
    active_window_index: u32,
    active_window_id: WindowId,
    policy: AttachedWindowSizePolicy,
    aggressive_resize: bool,
    linked_sessions: HashSet<(SessionName, SessionId)>,
    active_attach_epoch: u64,
    incoming_client_size: Option<TerminalSize>,
}

impl AttachedSizeSelection {
    fn still_exists(&self, session: &rmux_core::Session) -> bool {
        session.id() == self.session_id
            && session
                .window_at(self.active_window_index)
                .is_some_and(|window| window.id() == self.active_window_id)
    }
}

pub(in crate::handler) const ATTACHED_SIZE_RECONCILE_ATTEMPTS: usize = 4;

impl RequestHandler {
    pub(in crate::handler) async fn attached_window_size_policy_for_session(
        &self,
        session_name: &SessionName,
    ) -> Result<AttachedWindowSizePolicy, RmuxError> {
        let state = self.state.lock().await;
        let Some(session) = state.sessions.session(session_name) else {
            return Err(crate::pane_terminals::session_not_found(session_name));
        };
        let window_index = session.active_window_index();
        Ok(policy_from_option_value(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::WindowSize,
        )))
    }

    pub(in crate::handler) async fn attached_window_size_policy_for_session_identity(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
    ) -> Result<AttachedWindowSizePolicy, RmuxError> {
        let state = self.state.lock().await;
        let Some(session) = state
            .sessions
            .session(session_name)
            .filter(|session| session.id() == session_id)
        else {
            return Err(crate::pane_terminals::session_not_found(session_name));
        };
        let window_index = session.active_window_index();
        Ok(policy_from_option_value(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::WindowSize,
        )))
    }

    pub(in crate::handler) async fn reconcile_attached_session_size(
        &self,
        session_name: &SessionName,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let selection = self
                .selected_attached_session_size(session_name, None)
                .await?;
            self.pause_after_attached_size_selection().await;

            let mut state = self.state.lock().await;
            if state.sessions.session(session_name).is_none() {
                return Ok(None);
            }
            let active_attach = self.active_attach.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                session_name,
                &selection,
                true,
            ) {
                continue;
            }
            let Some(selected_size) = selection.selected_size else {
                return Ok(None);
            };
            if state
                .sessions
                .session(session_name)
                .expect("stable attached-size selection was revalidated")
                .window()
                .size()
                == selected_size
            {
                return Ok(None);
            }
            self.pause_before_attached_size_apply().await;
            state.mutate_session_and_resize_active_window_terminal(session_name, |session| {
                session.resize_active_window_terminal(selected_size);
                Ok(())
            })?;
            drop(active_attach);
            return Ok(Some(WindowTarget::with_window(
                session_name.clone(),
                selection.active_window_index,
            )));
        }
        Ok(None)
    }

    pub(in crate::handler) async fn reconcile_attached_session_size_and_emit(
        &self,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        if let Some(target) = self.reconcile_attached_session_size(session_name).await? {
            self.emit_without_attached_refresh(LifecycleEvent::WindowResized { target })
                .await;
        }
        Ok(())
    }

    pub(in crate::handler) async fn reconcile_attached_window_size(
        &self,
        target: &WindowTarget,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let selection = self.selected_attached_window_size(target, None).await?;
            let mut state = self.state.lock().await;
            if state.sessions.session(target.session_name()).is_none() {
                return Ok(None);
            }
            let active_attach = self.active_attach.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                target.session_name(),
                &selection,
                false,
            ) {
                continue;
            }
            let Some(selected_size) = selection.selected_size else {
                return Ok(None);
            };
            let current_size = state
                .sessions
                .session(target.session_name())
                .expect("stable attached-size session was revalidated")
                .window_at(target.window_index())
                .expect("stable window selection was revalidated")
                .size();
            if current_size == selected_size {
                return Ok(None);
            }
            self.pause_before_attached_size_apply().await;
            let window_index = target.window_index();
            state.mutate_session_and_resize_window_terminal(
                target.session_name(),
                window_index,
                |session| {
                    session.resize_window(window_index, selected_size)?;
                    Ok(())
                },
            )?;
            drop(active_attach);
            return Ok(Some(target.clone()));
        }
        Ok(None)
    }

    pub(in crate::handler) async fn reconcile_attached_window_size_and_emit(
        &self,
        target: &WindowTarget,
    ) -> Result<(), RmuxError> {
        if let Some(target) = self.reconcile_attached_window_size(target).await? {
            self.emit_without_attached_refresh(LifecycleEvent::WindowResized { target })
                .await;
        }
        Ok(())
    }

    pub(in crate::handler) async fn selected_attached_session_size_for_new_client(
        &self,
        session_name: &SessionName,
        client_size: TerminalSize,
        client_flags: super::ClientFlags,
    ) -> Result<AttachedSizeSelection, RmuxError> {
        if client_flags.contains(super::ClientFlags::IGNORESIZE) {
            return self
                .selected_attached_session_size(session_name, None)
                .await;
        }
        self.selected_attached_session_size(session_name, Some(client_size))
            .await
    }

    async fn selected_attached_session_size(
        &self,
        session_name: &SessionName,
        incoming_client_size: Option<TerminalSize>,
    ) -> Result<AttachedSizeSelection, RmuxError> {
        let (
            policy,
            aggressive_resize,
            linked_sessions,
            session_id,
            active_window_index,
            active_window_id,
        ) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return Err(crate::pane_terminals::session_not_found(session_name));
            };
            let active_window_index = session.active_window_index();
            let active_window_id = session.window().id();
            let policy = policy_from_option_value(state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::WindowSize,
            ));
            let aggressive_resize = state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::AggressiveResize,
            ) == Some("on");
            (
                policy,
                aggressive_resize,
                linked_session_identities(
                    &state,
                    session_name,
                    active_window_index,
                    aggressive_resize,
                ),
                session.id(),
                active_window_index,
                active_window_id,
            )
        };

        let (candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let candidates =
                attached_size_candidates(&active_attach, &linked_sessions, incoming_client_size);
            (candidates, self.active_attach_epoch.load(Ordering::Acquire))
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_attached_size(policy, &candidates),
            session_id,
            active_window_index,
            active_window_id,
            policy,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client_size,
        })
    }

    pub(in crate::handler) async fn selected_attached_window_size(
        &self,
        target: &WindowTarget,
        incoming_client_size: Option<TerminalSize>,
    ) -> Result<AttachedSizeSelection, RmuxError> {
        let (policy, aggressive_resize, linked_sessions, session_id, window_id) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(target.session_name())
                .ok_or_else(|| crate::pane_terminals::session_not_found(target.session_name()))?;
            let window = session.window_at(target.window_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
            let policy = policy_from_option_value(state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::WindowSize,
            ));
            let aggressive_resize = state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::AggressiveResize,
            ) == Some("on");
            (
                policy,
                aggressive_resize,
                linked_session_identities(
                    &state,
                    target.session_name(),
                    target.window_index(),
                    aggressive_resize,
                ),
                session.id(),
                window.id(),
            )
        };
        let (candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let candidates =
                attached_size_candidates(&active_attach, &linked_sessions, incoming_client_size);
            (candidates, self.active_attach_epoch.load(Ordering::Acquire))
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_attached_size(policy, &candidates),
            session_id,
            active_window_index: target.window_index(),
            active_window_id: window_id,
            policy,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client_size,
        })
    }

    pub(in crate::handler) fn attached_size_selection_is_current(
        &self,
        state: &crate::pane_terminals::HandlerState,
        active_attach: &super::ActiveAttachState,
        session_name: &SessionName,
        selection: &AttachedSizeSelection,
        require_active_window: bool,
    ) -> bool {
        if self.active_attach_epoch.load(Ordering::Acquire) != selection.active_attach_epoch {
            return false;
        }
        let Some(session) = state.sessions.session(session_name) else {
            return false;
        };
        if !selection.still_exists(session)
            || (require_active_window
                && session.active_window_index() != selection.active_window_index)
        {
            return false;
        }
        let policy = policy_from_option_value(state.options.resolve_for_window(
            session_name,
            selection.active_window_index,
            OptionName::WindowSize,
        ));
        let aggressive_resize = state.options.resolve_for_window(
            session_name,
            selection.active_window_index,
            OptionName::AggressiveResize,
        ) == Some("on");
        policy == selection.policy
            && aggressive_resize == selection.aggressive_resize
            && linked_session_identities(
                state,
                session_name,
                selection.active_window_index,
                aggressive_resize,
            ) == selection.linked_sessions
            && selected_attached_size(
                policy,
                &attached_size_candidates(
                    active_attach,
                    &selection.linked_sessions,
                    selection.incoming_client_size,
                ),
            ) == selection.selected_size
    }

    pub(in crate::handler) async fn prune_stale_attached_clients_for_session(
        &self,
        session_name: &SessionName,
    ) -> Vec<u32> {
        let stale_clients = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    &active.session_name == session_name
                        && (active.control_tx.is_closed()
                            || active.control_backlog.load(Ordering::Acquire)
                                >= super::ATTACH_CONTROL_BACKLOG_LIMIT)
                })
                .map(|(pid, active)| active.identity(*pid))
                .collect::<Vec<_>>()
        };
        self.remove_attached_clients_for_session(session_name, stale_clients)
            .await
    }

    pub(in crate::handler) async fn remove_attached_clients_for_session(
        &self,
        session_name: &SessionName,
        attach_identities: Vec<super::ActiveAttachIdentity>,
    ) -> Vec<u32> {
        if attach_identities.is_empty() {
            return Vec::new();
        }
        let (removed, key_tables, overlays) = {
            let mut active_attach = self.active_attach.lock().await;
            let mut removed = Vec::new();
            let mut key_tables = Vec::new();
            let mut overlays = Vec::new();
            for identity in attach_identities {
                let pid = identity.attach_pid();
                let remove = active_attach
                    .by_pid
                    .get(&pid)
                    .is_some_and(|active| identity.matches(pid, session_name, active));
                if remove {
                    let mut active = active_attach
                        .remove_attached_client(pid)
                        .expect("attached client checked above");
                    let _ = active.control_tx.send(AttachControl::Detach);
                    active.closing.store(true, Ordering::SeqCst);
                    removed.push(pid);
                    if let Some(table_name) = active.key_table_name.take() {
                        key_tables.push(table_name);
                    }
                    overlays.push(active.overlay.take());
                }
            }
            (removed, key_tables, overlays)
        };
        if !removed.is_empty() {
            self.bump_active_attach_epoch();
        }

        for overlay in overlays {
            super::terminate_overlay_job(overlay);
        }
        if !key_tables.is_empty() {
            let mut state = self.state.lock().await;
            for table_name in key_tables {
                state.key_bindings.unref_table(&table_name);
            }
        }
        for pid in &removed {
            self.emit_without_attached_refresh(LifecycleEvent::ClientDetached {
                session_name: session_name.clone(),
                client_name: Some(pid.to_string()),
            })
            .await;
        }
        removed
    }
}

fn attached_size_candidates(
    active_attach: &super::ActiveAttachState,
    linked_sessions: &HashSet<(SessionName, SessionId)>,
    incoming_client_size: Option<TerminalSize>,
) -> Vec<AttachedSizeCandidate> {
    let mut candidates = active_attach
        .by_pid
        .values()
        .filter(|active| {
            !active.suspended
                && !active.closing.load(Ordering::Acquire)
                && linked_sessions.contains(&(active.session_name.clone(), active.session_id))
                && !active.flags.contains(super::ClientFlags::IGNORESIZE)
        })
        .map(|active| AttachedSizeCandidate {
            size: active.client_size,
            sequence: active.size_sequence,
        })
        .collect::<Vec<_>>();
    if let Some(size) = incoming_client_size {
        candidates.push(AttachedSizeCandidate {
            size,
            sequence: active_attach.next_size_sequence,
        });
    }
    candidates
}

fn linked_session_identities(
    state: &crate::pane_terminals::HandlerState,
    session_name: &SessionName,
    window_index: u32,
    aggressive_resize: bool,
) -> HashSet<(SessionName, SessionId)> {
    let linked_sessions = if aggressive_resize {
        state.window_linked_current_sessions_list(session_name, window_index)
    } else {
        state.window_linked_sessions_list(session_name, window_index)
    };
    linked_sessions
        .into_iter()
        .filter_map(|linked_session_name| {
            state
                .sessions
                .session(&linked_session_name)
                .map(|linked_session| (linked_session_name, linked_session.id()))
        })
        .collect()
}

pub(in crate::handler) fn surviving_attached_resize_targets(
    state: &crate::pane_terminals::HandlerState,
    window_ids: impl IntoIterator<Item = WindowId>,
) -> Vec<WindowTarget> {
    let wanted = window_ids
        .into_iter()
        .map(WindowId::as_u32)
        .collect::<HashSet<_>>();
    let mut candidates = state
        .sessions
        .iter()
        .flat_map(|(session_name, session)| {
            session
                .windows()
                .iter()
                .filter(|(_, window)| wanted.contains(&window.id().as_u32()))
                .map(move |(window_index, window)| {
                    (
                        state.runtime_session_name_for_window(session_name, *window_index),
                        window.id().as_u32(),
                        WindowTarget::with_window(session_name.clone(), *window_index),
                    )
                })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.0
            .as_str()
            .cmp(right.0.as_str())
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| {
                left.2
                    .session_name()
                    .as_str()
                    .cmp(right.2.session_name().as_str())
            })
            .then_with(|| left.2.window_index().cmp(&right.2.window_index()))
    });

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|(runtime_session_name, window_id, target)| {
            seen.insert((runtime_session_name, window_id))
                .then_some(target)
        })
        .collect()
}

fn policy_from_option_value(value: Option<&str>) -> AttachedWindowSizePolicy {
    match value {
        Some("largest") => AttachedWindowSizePolicy::Largest,
        Some("smallest") => AttachedWindowSizePolicy::Smallest,
        Some("manual") => AttachedWindowSizePolicy::Manual,
        Some("latest") | None => AttachedWindowSizePolicy::Latest,
        Some(_) => AttachedWindowSizePolicy::Latest,
    }
}

fn selected_attached_size(
    policy: AttachedWindowSizePolicy,
    candidates: &[AttachedSizeCandidate],
) -> Option<TerminalSize> {
    match policy {
        AttachedWindowSizePolicy::Manual => None,
        AttachedWindowSizePolicy::Latest => candidates
            .iter()
            .max_by_key(|candidate| candidate.sequence)
            .map(|candidate| candidate.size),
        AttachedWindowSizePolicy::Largest => candidates
            .iter()
            .map(|candidate| candidate.size)
            .reduce(|selected, size| TerminalSize {
                cols: selected.cols.max(size.cols),
                rows: selected.rows.max(size.rows),
            }),
        AttachedWindowSizePolicy::Smallest => candidates
            .iter()
            .map(|candidate| candidate.size)
            .reduce(|selected, size| TerminalSize {
                cols: selected.cols.min(size.cols),
                rows: selected.rows.min(size.rows),
            }),
    }
}
