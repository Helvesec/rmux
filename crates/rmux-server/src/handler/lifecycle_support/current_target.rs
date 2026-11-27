use super::*;

pub(super) fn lifecycle_hook_current_target(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Option<Target> {
    if let LifecycleEvent::WindowUnlinked {
        session_name,
        window_id,
        ..
    } = event
    {
        return window_id
            .map(WindowId::new)
            .and_then(|window_id| resolve_global_window_target(state, window_id))
            .and_then(|target| active_window_target(&state.sessions, &target))
            .or_else(|| active_session_target(&state.sessions, session_name));
    }

    match event.current_target() {
        Some(Target::Session(session_name)) => {
            active_session_target(&state.sessions, &session_name)
        }
        Some(Target::Window(target)) => active_window_target(&state.sessions, &target)
            .or_else(|| active_session_target(&state.sessions, target.session_name())),
        Some(Target::Pane(target)) => {
            if matches!(event, LifecycleEvent::PaneExited { .. }) {
                return surviving_pane_event_target(state, &target);
            }
            let window_target =
                WindowTarget::with_window(target.session_name().clone(), target.window_index());
            let pane_exists = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .is_some();
            if pane_exists {
                Some(Target::Pane(target))
            } else {
                active_window_target(&state.sessions, &window_target)
                    .or_else(|| active_session_target(&state.sessions, target.session_name()))
            }
        }
        None => None,
    }
}

pub(super) fn lifecycle_hook_current_target_snapshot(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> (Option<Target>, Option<StableLifecycleTargetIdentity>) {
    let current_target = lifecycle_hook_current_target(state, event);
    let stable_current_target_identity = current_target
        .as_ref()
        .and_then(|target| StableLifecycleTargetIdentity::capture_for_event(state, target, event))
        .or_else(|| StableLifecycleTargetIdentity::capture_removed_event(event));
    (current_target, stable_current_target_identity)
}

/// The exact pane state tmux 3.7b captures when a notification fires
/// (`ne->fs`: session, winlink, window and its active pane).
///
/// `lifecycle_hook_current_target` returns that state while the event's own
/// object exists. It substitutes otherwise: `pane-exited` fires for a pane tmux
/// removes before any hook runs (`server_destroy_pane()`), and an unlinked
/// window has left the session that names it. A substitute is not a state.
pub(super) fn lifecycle_notification_state(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
    current_target: Option<&Target>,
) -> Option<StableLifecycleTargetIdentity> {
    if matches!(
        event,
        LifecycleEvent::PaneExited { .. } | LifecycleEvent::WindowUnlinked { .. }
    ) {
        return None;
    }
    let own_target = event.current_target()?;
    let Some(Target::Pane(pane)) = current_target else {
        return None;
    };
    let within_own_target = match &own_target {
        Target::Session(session_name) => pane.session_name() == session_name,
        Target::Window(window) => {
            pane.session_name() == window.session_name()
                && pane.window_index() == window.window_index()
        }
        Target::Pane(own_pane) => pane == own_pane,
    };
    if !within_own_target || !target_exists(state, &own_target) {
        return None;
    }
    StableLifecycleTargetIdentity::capture_with_window_loss(
        state,
        &Target::Pane(pane.clone()),
        StableWindowLoss::RequireWindowIdentity,
    )
}

pub(super) fn surviving_pane_event_target(
    state: &crate::pane_terminals::HandlerState,
    exiting_target: &PaneTarget,
) -> Option<Target> {
    let session = state.sessions.session(exiting_target.session_name())?;
    let Some(window) = session.window_at(exiting_target.window_index()) else {
        return fallback_session_pane_target_after_exit(state, exiting_target);
    };
    if let Some(active_pane) = window.active_pane() {
        if active_pane.index() != exiting_target.pane_index() {
            return Some(Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                exiting_target.window_index(),
                active_pane.index(),
            )));
        }
    }
    window
        .panes()
        .iter()
        .find(|pane| pane.index() != exiting_target.pane_index())
        .map(|pane| {
            Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                exiting_target.window_index(),
                pane.index(),
            ))
        })
        .or_else(|| fallback_session_pane_target_after_exit(state, exiting_target))
}

pub(super) fn fallback_session_pane_target_after_exit(
    state: &crate::pane_terminals::HandlerState,
    exiting_target: &PaneTarget,
) -> Option<Target> {
    if let Some(Target::Pane(target)) =
        active_session_target(&state.sessions, exiting_target.session_name())
    {
        if &target != exiting_target {
            return Some(Target::Pane(target));
        }
    }

    let session = state.sessions.session(exiting_target.session_name())?;
    session.windows().find_map(|(window_index, window)| {
        let target_for_pane = |pane_index| {
            Target::Pane(PaneTarget::with_window(
                exiting_target.session_name().clone(),
                window_index,
                pane_index,
            ))
        };
        if let Some(active_pane) = window.active_pane() {
            if window_index != exiting_target.window_index()
                || active_pane.index() != exiting_target.pane_index()
            {
                return Some(target_for_pane(active_pane.index()));
            }
        }
        window.panes().iter().find_map(|pane| {
            (window_index != exiting_target.window_index()
                || pane.index() != exiting_target.pane_index())
            .then(|| target_for_pane(pane.index()))
        })
    })
}

pub(super) fn first_session_pane_target(
    state: &crate::pane_terminals::HandlerState,
    session_name: &rmux_proto::SessionName,
) -> Option<Target> {
    let session = state.sessions.session(session_name)?;
    session.windows().find_map(|(window_index, window)| {
        window
            .active_pane()
            .or_else(|| window.panes().first())
            .map(|pane| {
                Target::Pane(PaneTarget::with_window(
                    session_name.clone(),
                    window_index,
                    pane.index(),
                ))
            })
    })
}

pub(super) fn hook_format_session_target(
    state: &crate::pane_terminals::HandlerState,
    formats: &[(String, String)],
) -> Option<Target> {
    let session_name = formats
        .iter()
        .rev()
        .find(|(name, _)| name == "hook_session_name")
        .and_then(|(_, value)| rmux_proto::SessionName::from_canonical(value.as_str()).ok())?;
    first_session_pane_target(state, &session_name)
}

pub(super) fn target_exists(state: &crate::pane_terminals::HandlerState, target: &Target) -> bool {
    match target {
        Target::Session(session_name) => state.sessions.session(session_name).is_some(),
        Target::Window(target) => state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some(),
        Target::Pane(target) => state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .is_some(),
    }
}

pub(super) fn pane_target_for_existing_target(
    state: &crate::pane_terminals::HandlerState,
    target: &Target,
) -> Option<Target> {
    match target {
        Target::Pane(target) => Some(Target::Pane(target.clone())),
        Target::Window(target) => {
            let session = state.sessions.session(target.session_name())?;
            let window = session.window_at(target.window_index())?;
            let pane = window.active_pane().or_else(|| window.panes().first())?;
            Some(Target::Pane(PaneTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
                pane.index(),
            )))
        }
        Target::Session(session_name) => first_session_pane_target(state, session_name),
    }
}
