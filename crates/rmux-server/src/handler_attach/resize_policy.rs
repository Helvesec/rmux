use std::collections::HashSet;
use std::sync::atomic::Ordering;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    OptionName, RmuxError, SessionId, SessionName, TerminalSize, WindowId, WindowTarget,
};

use crate::client_names::attached_client_name;
use crate::pane_io::AttachControl;

use super::super::{client_support::SwitchTargetSelection, RequestHandler};

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ControlSizeCandidate {
    pid: u32,
    control_id: u64,
    session_name: SessionName,
    session_id: SessionId,
    size: TerminalSize,
    sequence: u64,
}

impl ControlSizeCandidate {
    const fn size_candidate(&self) -> AttachedSizeCandidate {
        AttachedSizeCandidate {
            size: self.size,
            sequence: self.sequence,
        }
    }
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
    control_candidates: Vec<ControlSizeCandidate>,
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

#[derive(Clone, Copy)]
pub(in crate::handler) struct ControlResizeClient<'a> {
    active_attach: &'a super::ActiveAttachState,
    active_control: &'a super::super::ActiveControlState,
    control_pid: u32,
    /// `None` while the client has never announced a size with
    /// `refresh-client -C`; such a client owns no window geometry, exactly as
    /// tmux's `ignore_client_size()` skips it.
    declared_size: Option<TerminalSize>,
    size_sequence: u64,
}

impl<'a> ControlResizeClient<'a> {
    pub(in crate::handler) const fn new(
        active_attach: &'a super::ActiveAttachState,
        active_control: &'a super::super::ActiveControlState,
        control_pid: u32,
        declared_size: Option<TerminalSize>,
        size_sequence: u64,
    ) -> Self {
        Self {
            active_attach,
            active_control,
            control_pid,
            declared_size,
            size_sequence,
        }
    }
}

/// Applies a control client's reported size to its session.
///
/// Any window this really resizes is recorded by the geometry chokepoint and
/// published by `RequestHandler::publish_applied_window_resizes`; callers must
/// never publish a layout notification of their own.
pub(in crate::handler) fn resize_control_session_for_client(
    state: &mut crate::pane_terminals::HandlerState,
    client: ControlResizeClient<'_>,
    session_name: &SessionName,
    expected_session_id: SessionId,
) -> Result<(), RmuxError> {
    let window_index = state
        .sessions
        .session(session_name)
        .filter(|session| session.id() == expected_session_id)
        .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?
        .active_window_index();
    let (current_size, selected_size) = control_resize_selection(
        state,
        client,
        session_name,
        expected_session_id,
        window_index,
    )?;
    let Some(selected_size) = selected_size else {
        return Ok(());
    };
    if current_size == selected_size {
        return Ok(());
    }

    state.mutate_session_and_resize_active_window_terminal(session_name, |session| {
        if session.id() != expected_session_id {
            return Err(crate::pane_terminals::session_not_found(session_name));
        }
        session.touch_attached();
        session.resize_active_window_terminal(selected_size);
        Ok(())
    })
}

/// Moves a control client to `session_name` and applies the geometry that move
/// implies.
///
/// Any window this really resizes is recorded by the geometry chokepoint and
/// published by `RequestHandler::publish_applied_window_resizes`; callers must
/// never publish a layout notification of their own.
pub(in crate::handler) fn switch_control_session_for_client(
    state: &mut crate::pane_terminals::HandlerState,
    client: ControlResizeClient<'_>,
    session_name: &SessionName,
    expected_session_id: SessionId,
    selection: Option<&SwitchTargetSelection>,
) -> Result<Vec<SessionName>, RmuxError> {
    let window_index = match selection {
        Some(selection) => selection.window_target().window_index(),
        None => state
            .sessions
            .session(session_name)
            .filter(|session| session.id() == expected_session_id)
            .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?
            .active_window_index(),
    };
    let (current_size, selected_size) = control_resize_selection(
        state,
        client,
        session_name,
        expected_session_id,
        window_index,
    )?;
    let resizes = selected_size.is_some_and(|selected_size| selected_size != current_size);
    if selection.is_none() && !resizes {
        return Ok(Vec::new());
    }

    let (_, refresh_sessions) = state.mutate_session_and_resize_window_terminal_with_family(
        session_name,
        window_index,
        |session| {
            if session.id() != expected_session_id {
                return Err(crate::pane_terminals::session_not_found(session_name));
            }
            if let Some(selection) = selection {
                selection.apply_to_session(session)?;
            }
            if let Some(selected_size) = selected_size {
                session.resize_window(window_index, selected_size)?;
            }
            Ok(())
        },
    )?;
    Ok(refresh_sessions)
}

fn control_resize_selection(
    state: &crate::pane_terminals::HandlerState,
    client: ControlResizeClient<'_>,
    session_name: &SessionName,
    expected_session_id: SessionId,
    window_index: u32,
) -> Result<(TerminalSize, Option<TerminalSize>), RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .filter(|session| session.id() == expected_session_id)
        .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?;
    let current_size = session
        .window_at(window_index)
        .ok_or_else(|| RmuxError::invalid_target(window_index.to_string(), "window not found"))?
        .size();
    let policy = policy_from_option_value(state.options.resolve_for_window(
        session_name,
        window_index,
        OptionName::WindowSize,
    ));
    let aggressive_resize =
        state
            .options
            .resolve_for_window(session_name, window_index, OptionName::AggressiveResize)
            == Some("on");
    let linked_sessions =
        linked_session_identities(state, session_name, window_index, aggressive_resize);
    let candidates = attached_size_candidates(
        client.active_attach,
        &linked_sessions,
        client.declared_size.map(|size| AttachedSizeCandidate {
            size,
            sequence: client.size_sequence,
        }),
    );
    let control_candidates = control_size_candidates(
        client.active_control,
        &linked_sessions,
        Some(client.control_pid),
    );
    Ok((
        current_size,
        selected_client_size(policy, candidates, &control_candidates),
    ))
}

/// The control clients that own a size for the window-size policy.
///
/// A control client is only a candidate once it has announced a size with
/// `refresh-client -C`: tmux 3.7b's `ignore_client_size()` skips a
/// `CLIENT_CONTROL` client without `CLIENT_SIZECHANGED`, so a freshly attached
/// control client — the state every control client starts in, including
/// iTerm2's `-CC` before its first `refresh-client -C` — must not pull the
/// session down to its 80x24 placeholder.
fn control_size_candidates(
    active_control: &super::super::ActiveControlState,
    linked_sessions: &HashSet<(SessionName, SessionId)>,
    excluded_pid: Option<u32>,
) -> Vec<ControlSizeCandidate> {
    let mut candidates = active_control
        .by_pid
        .iter()
        .filter(|(pid, active)| {
            Some(**pid) != excluded_pid
                && active.size_declared
                && !active.closing.load(Ordering::Acquire)
                && active
                    .session_name
                    .as_ref()
                    .zip(active.session_id)
                    .is_some_and(|(session_name, session_id)| {
                        linked_sessions.iter().any(|(linked_name, linked_id)| {
                            linked_name == session_name && *linked_id == session_id
                        })
                    })
        })
        .filter_map(|(&pid, active)| {
            let (session_name, session_id) = active.session_name.clone().zip(active.session_id)?;
            Some(ControlSizeCandidate {
                pid,
                control_id: active.id,
                session_name,
                session_id,
                size: TerminalSize {
                    cols: active.client_width,
                    rows: active.client_height,
                },
                sequence: active.size_sequence,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| (candidate.pid, candidate.control_id));
    candidates
}

fn selected_client_size(
    policy: AttachedWindowSizePolicy,
    mut attached_candidates: Vec<AttachedSizeCandidate>,
    control_candidates: &[ControlSizeCandidate],
) -> Option<TerminalSize> {
    attached_candidates.extend(
        control_candidates
            .iter()
            .map(ControlSizeCandidate::size_candidate),
    );
    selected_attached_size(policy, &attached_candidates)
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
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
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
            drop(active_control);
            drop(active_attach);
            return Ok(Some(WindowTarget::with_window(
                session_name.clone(),
                selection.active_window_index,
            )));
        }
        Ok(None)
    }

    /// Publishes what tmux publishes for every window that really did change
    /// size since the last publication.
    ///
    /// tmux 3.7b's `resize_window()` runs `window-layout-changed` and then
    /// `window-resized` for every applied resize, and only the former reaches
    /// control clients (as `%layout-change`). Measured 2026-07-25 on a source
    /// session holding a 101x41 PTY client and a 60x20 control client with
    /// `window-size largest`: after `detach-client` on the PTY client, the
    /// control client receives
    ///     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    /// then `%client-detached /dev/ttys007`; when the PTY client's process dies
    /// instead, the same `%layout-change` arrives after `%client-detached`.
    /// All tickets are reserved under one state lock so they publish in tmux's
    /// order.
    pub(in crate::handler) async fn publish_applied_window_resizes(&self) {
        let prepared = {
            let mut state = self.state.lock().await;
            prepare_applied_window_resize_events(&mut state)
        };
        for event in prepared {
            self.emit_prepared(event).await;
        }
    }

    /// Publishes `target` as an applied resize even when its stored size did not
    /// move, for the commands whose tmux counterpart calls `resize_window()`
    /// unconditionally (`resize-window`).
    pub(in crate::handler) async fn emit_applied_window_resize(&self, target: WindowTarget) {
        let prepared = {
            let mut state = self.state.lock().await;
            state.record_applied_window_resize(target);
            prepare_applied_window_resize_events(&mut state)
        };
        for event in prepared {
            self.emit_prepared(event).await;
        }
    }

    pub(in crate::handler) async fn reconcile_attached_session_size_and_emit(
        &self,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        self.reconcile_attached_session_size(session_name).await?;
        self.publish_applied_window_resizes().await;
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
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
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
            drop(active_control);
            drop(active_attach);
            return Ok(Some(target.clone()));
        }
        Ok(None)
    }

    pub(in crate::handler) async fn reconcile_attached_window_size_and_emit(
        &self,
        target: &WindowTarget,
    ) -> Result<(), RmuxError> {
        self.reconcile_attached_window_size(target).await?;
        self.publish_applied_window_resizes().await;
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

        let incoming_candidate = incoming_client_size.map(|size| AttachedSizeCandidate {
            size,
            sequence: self.current_client_size_sequence(),
        });
        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates =
                attached_size_candidates(&active_attach, &linked_sessions, incoming_candidate);
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_client_size(policy, candidates, &control_candidates),
            session_id,
            active_window_index,
            active_window_id,
            policy,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client_size,
            control_candidates,
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
        let incoming_candidate = incoming_client_size.map(|size| AttachedSizeCandidate {
            size,
            sequence: self.current_client_size_sequence(),
        });
        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates =
                attached_size_candidates(&active_attach, &linked_sessions, incoming_candidate);
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_client_size(policy, candidates, &control_candidates),
            session_id,
            active_window_index: target.window_index(),
            active_window_id: window_id,
            policy,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client_size,
            control_candidates,
        })
    }

    pub(in crate::handler) fn attached_size_selection_is_current(
        &self,
        state: &crate::pane_terminals::HandlerState,
        active_attach: &super::ActiveAttachState,
        active_control: &super::super::ActiveControlState,
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
        let current_control_candidates =
            control_size_candidates(active_control, &selection.linked_sessions, None);
        let incoming_candidate = selection
            .incoming_client_size
            .map(|size| AttachedSizeCandidate {
                size,
                sequence: self.current_client_size_sequence(),
            });
        policy == selection.policy
            && aggressive_resize == selection.aggressive_resize
            && linked_session_identities(
                state,
                session_name,
                selection.active_window_index,
                aggressive_resize,
            ) == selection.linked_sessions
            && current_control_candidates == selection.control_candidates
            && selected_client_size(
                policy,
                attached_size_candidates(
                    active_attach,
                    &selection.linked_sessions,
                    incoming_candidate,
                ),
                &current_control_candidates,
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
                client_name: Some(attached_client_name(*pid)),
            })
            .await;
        }
        removed
    }
}

/// Reserves the `window-layout-changed` / `window-resized` pair for every
/// window the geometry chokepoint recorded, in tmux's order, under one state
/// lock.
pub(in crate::handler) fn prepare_applied_window_resize_events(
    state: &mut crate::pane_terminals::HandlerState,
) -> Vec<crate::handler::QueuedLifecycleEvent> {
    let targets = state.take_applied_window_resizes();
    let mut prepared = Vec::with_capacity(targets.len().saturating_mul(2));
    for target in targets {
        prepared.push(crate::handler::prepare_lifecycle_event(
            state,
            &LifecycleEvent::WindowLayoutChanged {
                target: target.clone(),
            },
        ));
        prepared.push(crate::handler::prepare_lifecycle_event(
            state,
            &LifecycleEvent::WindowResized { target },
        ));
    }
    prepared
}

fn attached_size_candidates(
    active_attach: &super::ActiveAttachState,
    linked_sessions: &HashSet<(SessionName, SessionId)>,
    incoming_client: Option<AttachedSizeCandidate>,
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
    if let Some(incoming_client) = incoming_client {
        candidates.push(incoming_client);
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
