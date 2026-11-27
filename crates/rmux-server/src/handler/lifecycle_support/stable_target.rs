use super::*;

impl StableLifecycleTargetIdentity {
    pub(super) fn capture_for_event(
        state: &crate::pane_terminals::HandlerState,
        target: &Target,
        event: &LifecycleEvent,
    ) -> Option<Self> {
        let window_loss = if matches!(event, LifecycleEvent::WindowUnlinked { .. }) {
            StableWindowLoss::RequireWindowIdentity
        } else {
            StableWindowLoss::FallbackToSession
        };
        Self::capture_with_window_loss(state, target, window_loss)
    }

    pub(super) fn capture_with_window_loss(
        state: &crate::pane_terminals::HandlerState,
        target: &Target,
        window_loss: StableWindowLoss,
    ) -> Option<Self> {
        let session = state.sessions.session(target.session_name())?;
        let session_id = session.id();
        match target {
            Target::Session(_) => Some(Self::Session { session_id }),
            Target::Window(target) => Some(Self::Window {
                session_id,
                window: stable_window_identity(
                    state,
                    target.session_name(),
                    session,
                    target.window_index(),
                    window_loss,
                )?,
                window_loss,
            }),
            Target::Pane(target) => {
                let window = session.window_at(target.window_index())?;
                let pane_id = window.pane(target.pane_index())?.id();
                Some(Self::Pane {
                    session_id,
                    window: stable_window_identity(
                        state,
                        target.session_name(),
                        session,
                        target.window_index(),
                        window_loss,
                    )?,
                    pane_id,
                    window_loss,
                })
            }
        }
    }

    pub(super) fn resolve(&self, state: &crate::pane_terminals::HandlerState) -> Option<Target> {
        match self {
            Self::Session { session_id } => resolve_stable_session_target(state, *session_id),
            Self::Window {
                session_id,
                window,
                window_loss,
            } => resolve_stable_window_target(state, *session_id, *window)
                .map(Target::Window)
                .or_else(|| resolve_stable_window_loss(state, *session_id, *window, *window_loss)),
            Self::Pane {
                session_id,
                window,
                pane_id,
                window_loss,
            } => resolve_stable_pane_target(state, *session_id, *window, *pane_id)
                .map(Target::Pane)
                .or_else(|| {
                    resolve_stable_window_target(state, *session_id, *window).map(Target::Window)
                })
                .or_else(|| resolve_stable_window_loss(state, *session_id, *window, *window_loss)),
            Self::GlobalWindow { window_id } => {
                resolve_global_window_target(state, *window_id).map(Target::Window)
            }
            Self::GlobalPane { pane_id, window_id } => resolve_global_pane_target(state, *pane_id)
                .map(Target::Pane)
                .or_else(|| {
                    window_id.and_then(|window_id| {
                        resolve_global_window_target(state, window_id).map(Target::Window)
                    })
                }),
            Self::Removed => None,
        }
    }

    /// tmux 3.7b `cmd_find_valid_state()`: the captured session still holds
    /// the captured window, and that window still holds the captured pane.
    pub(super) fn holds_live_pane_state(
        &self,
        state: &crate::pane_terminals::HandlerState,
    ) -> bool {
        let Self::Pane {
            session_id,
            window,
            pane_id,
            ..
        } = self
        else {
            return false;
        };
        resolve_stable_pane_target(state, *session_id, *window, *pane_id).is_some_and(|target| {
            state
                .sessions
                .session(target.session_name())
                .is_some_and(|session| session.id() == *session_id)
        })
    }

    pub(super) fn capture_removed_event(event: &LifecycleEvent) -> Option<Self> {
        match event {
            LifecycleEvent::SessionClosed { session_id, .. } => Some(match session_id {
                Some(session_id) => Self::Session {
                    session_id: SessionId::new(*session_id),
                },
                None => Self::Removed,
            }),
            LifecycleEvent::WindowUnlinked { window_id, .. } => Some(match window_id {
                Some(window_id) => Self::GlobalWindow {
                    window_id: WindowId::new(*window_id),
                },
                None => Self::Removed,
            }),
            LifecycleEvent::PaneExited {
                pane_id, window_id, ..
            }
            | LifecycleEvent::PaneDied {
                pane_id, window_id, ..
            } => Some(match (pane_id, window_id) {
                (Some(pane_id), window_id) => Self::GlobalPane {
                    pane_id: PaneId::new(*pane_id),
                    window_id: window_id.map(WindowId::new),
                },
                (None, Some(window_id)) => Self::GlobalWindow {
                    window_id: WindowId::new(*window_id),
                },
                (None, None) => Self::Removed,
            }),
            _ => None,
        }
    }
}

pub(super) fn exact_pane_hook_target(
    state: &crate::pane_terminals::HandlerState,
    identity: ExactPaneHookTarget,
) -> Option<Target> {
    let target = if let Some(occurrence_id) = identity.window_occurrence_id {
        state.window_link_occurrence_target(
            occurrence_id,
            identity.session_id,
            identity.window_id,
        )?
    } else {
        let (session_name, session) = state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
            .find(|(_, session)| session.id() == identity.session_id)?;
        let (window_index, _) = session
            .windows()
            .filter(|(_, window)| window.id() == identity.window_id)
            .min_by_key(|(window_index, _)| {
                (
                    *window_index != identity.preferred_window_index,
                    *window_index,
                )
            })?;
        WindowTarget::with_window(session_name.clone(), window_index)
    };
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    window
        .panes()
        .iter()
        .filter(|pane| pane.id() == identity.pane_id)
        .min_by_key(|pane| pane.index())
        .map(|pane| {
            Target::Pane(PaneTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
                pane.index(),
            ))
        })
}

pub(super) fn resolve_stable_window_loss(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    window: StableWindowIdentity,
    window_loss: StableWindowLoss,
) -> Option<Target> {
    match window_loss {
        StableWindowLoss::FallbackToSession => {
            resolve_stable_session_parent_target(state, session_id, window)
        }
        StableWindowLoss::RequireWindowIdentity => None,
    }
}

pub(super) fn resolve_stable_session_parent_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    child_window: StableWindowIdentity,
) -> Option<Target> {
    if stable_window_slot_was_replaced(state, session_id, child_window) {
        return None;
    }
    resolve_stable_session_target(state, session_id)
}

pub(super) fn stable_window_slot_was_replaced(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    identity: StableWindowIdentity,
) -> bool {
    let Some((session_name, session)) = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find(|(_, session)| session.id() == session_id)
    else {
        return false;
    };
    session
        .window_at(identity.preferred_index)
        .is_some_and(|window| {
            window.id() != identity.window_id
                || identity.occurrence_id.is_some_and(|occurrence_id| {
                    state.window_link_occurrence_id(session_name, identity.preferred_index)
                        != Some(occurrence_id)
                })
        })
}

pub(super) fn resolve_stable_session_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
) -> Option<Target> {
    state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find(|(_, session)| session.id() == session_id)
        .map(|(session_name, _)| Target::Session(session_name.clone()))
}

pub(super) fn resolve_global_window_target(
    state: &crate::pane_terminals::HandlerState,
    window_id: WindowId,
) -> Option<WindowTarget> {
    state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .filter_map(|(session_name, session)| {
            first_window_target_with_id(session_name, session, window_id)
                .map(|target| (session.id(), target))
        })
        .max_by_key(|(session_id, _)| *session_id)
        .map(|(_, target)| target)
}

pub(super) fn resolve_global_pane_target(
    state: &crate::pane_terminals::HandlerState,
    pane_id: PaneId,
) -> Option<PaneTarget> {
    state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .filter_map(|(session_name, session)| {
            first_pane_target_with_id(session_name, session, pane_id)
                .map(|target| (session.id(), target))
        })
        .max_by_key(|(session_id, _)| *session_id)
        .map(|(_, target)| target)
}

pub(super) fn first_window_target_with_id(
    session_name: &rmux_proto::SessionName,
    session: rmux_core::ManagedSessionView<'_>,
    window_id: WindowId,
) -> Option<WindowTarget> {
    session
        .windows()
        .find(|(_, window)| window.id() == window_id)
        .map(|(window_index, _)| WindowTarget::with_window(session_name.clone(), window_index))
}

pub(super) fn first_pane_target_with_id(
    session_name: &rmux_proto::SessionName,
    session: rmux_core::ManagedSessionView<'_>,
    pane_id: PaneId,
) -> Option<PaneTarget> {
    session.windows().find_map(|(window_index, window)| {
        window
            .panes()
            .iter()
            .filter(|pane| pane.id() == pane_id)
            .min_by_key(|pane| pane.index())
            .map(|pane| PaneTarget::with_window(session_name.clone(), window_index, pane.index()))
    })
}

pub(super) fn resolve_stable_window_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    identity: StableWindowIdentity,
) -> Option<WindowTarget> {
    if let Some(occurrence_id) = identity.occurrence_id {
        return state.window_link_occurrence_target(occurrence_id, session_id, identity.window_id);
    }
    let original_session = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find(|(_, session)| session.id() == session_id);
    if let Some((session_name, session)) = original_session {
        if let Some((window_index, _)) = resolve_stable_window_identity(session, identity) {
            return Some(WindowTarget::with_window(
                session_name.clone(),
                window_index,
            ));
        }
    }

    resolve_global_window_target(state, identity.window_id)
}

pub(super) fn resolve_stable_pane_target(
    state: &crate::pane_terminals::HandlerState,
    session_id: SessionId,
    window_identity: StableWindowIdentity,
    pane_id: PaneId,
) -> Option<PaneTarget> {
    if window_identity.occurrence_id.is_some() {
        let window_target = resolve_stable_window_target(state, session_id, window_identity)?;
        let pane_index = state
            .sessions
            .session(window_target.session_name())?
            .window_at(window_target.window_index())?
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)?
            .index();
        return Some(PaneTarget::with_window(
            window_target.session_name().clone(),
            window_target.window_index(),
            pane_index,
        ));
    }

    let pane_in_session = |session_name: &rmux_proto::SessionName,
                           session: rmux_core::ManagedSessionView<'_>|
     -> Option<PaneTarget> {
        let (window_index, window) = resolve_stable_window_identity(session, window_identity)?;
        let pane_index = window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)?
            .index();
        Some(PaneTarget::with_window(
            session_name.clone(),
            window_index,
            pane_index,
        ))
    };

    if let Some(target) = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find(|(_, session)| session.id() == session_id)
        .and_then(|(session_name, session)| pane_in_session(session_name, session))
    {
        return Some(target);
    }

    resolve_global_pane_target(state, pane_id)
}

pub(super) fn stable_window_identity(
    state: &crate::pane_terminals::HandlerState,
    session_name: &rmux_proto::SessionName,
    session: rmux_core::ManagedSessionView<'_>,
    window_index: u32,
    window_loss: StableWindowLoss,
) -> Option<StableWindowIdentity> {
    let window_id = session.window_at(window_index)?.id();
    Some(StableWindowIdentity {
        window_id,
        preferred_index: window_index,
        occurrence_id: if matches!(window_loss, StableWindowLoss::RequireWindowIdentity) {
            state.window_link_occurrence_id(session_name, window_index)
        } else {
            None
        },
    })
}

pub(super) fn resolve_stable_window_identity(
    session: rmux_core::ManagedSessionView<'_>,
    identity: StableWindowIdentity,
) -> Option<(u32, &rmux_core::Window)> {
    if let Some(window) = session
        .window_at(identity.preferred_index)
        .filter(|window| window.id() == identity.window_id)
    {
        return Some((identity.preferred_index, window));
    }

    session.windows().find_map(|(window_index, window)| {
        (window.id() == identity.window_id).then_some((window_index, window))
    })
}
