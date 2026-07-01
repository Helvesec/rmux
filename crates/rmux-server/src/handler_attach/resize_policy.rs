use std::collections::HashSet;
use std::sync::atomic::Ordering;

use rmux_core::LifecycleEvent;
use rmux_proto::{OptionName, RmuxError, SessionName, TerminalSize, WindowTarget};

use crate::pane_io::AttachControl;

use super::super::RequestHandler;

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

    pub(in crate::handler) async fn reconcile_attached_session_size(
        &self,
        session_name: &SessionName,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        let (selected_size, current_size, active_window_index) = self
            .selected_attached_session_size(session_name, None)
            .await?;
        let Some(selected_size) = selected_size else {
            return Ok(None);
        };
        if selected_size == current_size {
            return Ok(None);
        }

        let mut state = self.state.lock().await;
        let Some(session) = state.sessions.session(session_name) else {
            return Ok(None);
        };
        if session.window().size() == selected_size {
            return Ok(None);
        }
        state.mutate_session_and_resize_terminals(session_name, |session| {
            session.resize_terminal(selected_size);
            Ok(())
        })?;
        Ok(Some(WindowTarget::with_window(
            session_name.clone(),
            active_window_index,
        )))
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

    pub(in crate::handler) async fn selected_attached_session_size_for_new_client(
        &self,
        session_name: &SessionName,
        client_size: TerminalSize,
        client_flags: super::ClientFlags,
    ) -> Result<Option<TerminalSize>, RmuxError> {
        if client_flags.contains(super::ClientFlags::IGNORESIZE) {
            let (selected_size, _current_size, _active_window_index) = self
                .selected_attached_session_size(session_name, None)
                .await?;
            return Ok(selected_size);
        }
        let (selected_size, _current_size, _active_window_index) = self
            .selected_attached_session_size(session_name, Some(client_size))
            .await?;
        Ok(selected_size)
    }

    async fn selected_attached_session_size(
        &self,
        session_name: &SessionName,
        incoming_client_size: Option<TerminalSize>,
    ) -> Result<(Option<TerminalSize>, TerminalSize, u32), RmuxError> {
        let (policy, linked_sessions, current_size, active_window_index) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return Err(crate::pane_terminals::session_not_found(session_name));
            };
            let active_window_index = session.active_window_index();
            let policy = policy_from_option_value(state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::WindowSize,
            ));
            if policy == AttachedWindowSizePolicy::Manual {
                return Ok((None, session.window().size(), active_window_index));
            }
            let aggressive_resize = state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::AggressiveResize,
            ) == Some("on");
            let linked_sessions = if aggressive_resize {
                state.window_linked_current_sessions_list(session_name, active_window_index)
            } else {
                state.window_linked_sessions_list(session_name, active_window_index)
            };
            (
                policy,
                linked_sessions.into_iter().collect::<HashSet<_>>(),
                session.window().size(),
                active_window_index,
            )
        };

        let candidates = {
            let active_attach = self.active_attach.lock().await;
            let mut candidates = active_attach
                .by_pid
                .values()
                .filter(|active| {
                    !active.suspended
                        && !active.closing.load(Ordering::Acquire)
                        && linked_sessions.contains(&active.session_name)
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
        };
        Ok((
            selected_attached_size(policy, &candidates),
            current_size,
            active_window_index,
        ))
    }

    pub(in crate::handler) async fn prune_stale_attached_clients_for_session(
        &self,
        session_name: &SessionName,
    ) -> Vec<u32> {
        let stale_pids = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (&active.session_name == session_name
                        && (active.control_tx.is_closed()
                            || active.control_backlog.load(Ordering::Acquire)
                                >= super::ATTACH_CONTROL_BACKLOG_LIMIT))
                        .then_some(*pid)
                })
                .collect::<Vec<_>>()
        };
        self.remove_attached_clients_for_session(session_name, stale_pids)
            .await
    }

    pub(in crate::handler) async fn remove_attached_clients_for_session(
        &self,
        session_name: &SessionName,
        attach_pids: Vec<u32>,
    ) -> Vec<u32> {
        if attach_pids.is_empty() {
            return Vec::new();
        }
        let (removed, key_tables, overlays) = {
            let mut active_attach = self.active_attach.lock().await;
            let mut removed = Vec::new();
            let mut key_tables = Vec::new();
            let mut overlays = Vec::new();
            for pid in attach_pids {
                let remove = active_attach
                    .by_pid
                    .get(&pid)
                    .is_some_and(|active| &active.session_name == session_name);
                if remove {
                    let mut active = active_attach
                        .by_pid
                        .remove(&pid)
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
