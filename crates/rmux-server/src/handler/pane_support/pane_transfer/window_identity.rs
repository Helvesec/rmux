use rmux_core::WindowId;
use rmux_proto::{BreakPaneRequest, PaneTarget, SessionName, WindowTarget};

use crate::pane_terminals::HandlerState;

pub(super) struct BreakSourceWindowIdentity {
    session_name: SessionName,
    window_id: WindowId,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct PaneTransferWindowIdentity {
    session_name: SessionName,
    window_id: WindowId,
}

impl PaneTransferWindowIdentity {
    pub(super) fn capture(state: &HandlerState, target: &PaneTarget) -> Option<Self> {
        let session = state.sessions.session(target.session_name())?;
        let window = session.window_at(target.window_index())?;
        Some(Self {
            session_name: target.session_name().clone(),
            window_id: window.id(),
        })
    }

    fn resolve(&self, state: &HandlerState) -> Option<WindowTarget> {
        let session = state.sessions.session(&self.session_name)?;
        session.windows().find_map(|(window_index, window)| {
            (window.id() == self.window_id)
                .then(|| WindowTarget::with_window(self.session_name.clone(), window_index))
        })
    }
}

impl BreakSourceWindowIdentity {
    pub(super) fn capture(state: &HandlerState, request: &BreakPaneRequest) -> Option<Self> {
        let source_session = state.sessions.session(request.source.session_name())?;
        let source_window = source_session.window_at(request.source.window_index())?;
        (source_window.pane_count() > 1).then_some(Self {
            session_name: request.source.session_name().clone(),
            window_id: source_window.id(),
        })
    }

    pub(super) fn resolve_after_break(&self, state: &HandlerState) -> Option<WindowTarget> {
        let session = state.sessions.session(&self.session_name)?;
        session.windows().find_map(|(window_index, window)| {
            (window.id() == self.window_id)
                .then(|| WindowTarget::with_window(self.session_name.clone(), window_index))
        })
    }
}

pub(super) struct SwapLayoutSnapshot {
    identity: PaneTransferWindowIdentity,
    layout: String,
    visible_layout: String,
    zoomed: bool,
}

impl SwapLayoutSnapshot {
    pub(super) fn capture(state: &HandlerState, target: &WindowTarget) -> Option<Self> {
        let window = state.sessions.window(target).ok()?;
        Some(Self {
            identity: PaneTransferWindowIdentity {
                session_name: target.session_name().clone(),
                window_id: window.id(),
            },
            layout: window.layout_dump(),
            visible_layout: window.visible_layout_dump(),
            zoomed: window.is_zoomed(),
        })
    }

    pub(super) fn changed_target(&self, state: &HandlerState) -> Option<WindowTarget> {
        let target = self.identity.resolve(state)?;
        let window = state.sessions.window(&target).ok()?;
        (window.is_zoomed() != self.zoomed
            || window.layout_dump() != self.layout
            || window.visible_layout_dump() != self.visible_layout)
            .then_some(target)
    }
}

pub(super) fn swap_layout_targets(
    state: &HandlerState,
    source: &WindowTarget,
    target: &WindowTarget,
    source_identity: Option<&PaneTransferWindowIdentity>,
    target_identity: Option<&PaneTransferWindowIdentity>,
) -> Vec<WindowTarget> {
    let resolved_target = target_identity
        .and_then(|identity| identity.resolve(state))
        .unwrap_or_else(|| target.clone());
    if source_identity
        .zip(target_identity)
        .is_some_and(|(source, target)| source.window_id == target.window_id)
    {
        return vec![resolved_target];
    }

    let resolved_source = source_identity
        .and_then(|identity| identity.resolve(state))
        .unwrap_or_else(|| source.clone());
    if source == target {
        vec![resolved_target]
    } else {
        vec![resolved_source, resolved_target]
    }
}

pub(super) fn join_layout_targets(
    state: &HandlerState,
    source: &WindowTarget,
    target: &WindowTarget,
    source_identity: Option<&PaneTransferWindowIdentity>,
    target_identity: Option<&PaneTransferWindowIdentity>,
) -> Vec<WindowTarget> {
    let resolved_target = target_identity
        .and_then(|identity| identity.resolve(state))
        .unwrap_or_else(|| target.clone());
    let same_window_identity = source_identity
        .zip(target_identity)
        .is_some_and(|(source, target)| source.window_id == target.window_id);
    let resolved_source = if same_window_identity {
        resolved_target.clone()
    } else {
        source_identity
            .and_then(|identity| identity.resolve(state))
            .unwrap_or_else(|| resolved_target.clone())
    };

    if source == target {
        vec![resolved_target]
    } else {
        vec![resolved_source, resolved_target]
    }
}
