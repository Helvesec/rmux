use rmux_core::{KillPaneOutcome, ManagedSessionStore, PaneGeometry, PaneId};
use rmux_proto::{
    PaneTarget, RmuxError, SessionName, SplitDirection, SplitWindowTarget, WindowTarget,
};

use super::super::session_not_found;

pub(super) fn preview_split(
    sessions: &ManagedSessionStore,
    target: &SplitWindowTarget,
    direction: SplitDirection,
    before: bool,
) -> Result<(u32, u32, PaneGeometry), RmuxError> {
    let session_name = split_window_session_name(target);
    let session = sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let window_index = split_window_target_window_index(session, target);
    let pane_index = match target {
        SplitWindowTarget::Session(_) => session.active_pane_index(),
        SplitWindowTarget::Pane(target) => target.pane_index(),
    };
    let (index, geometry) = sessions.preview_split_pane(
        &WindowTarget::with_window(session_name.clone(), window_index),
        pane_index,
        direction,
        before,
    )?;
    Ok((window_index, index, geometry))
}

/// The window a tiled split changes and the panes it resizes there: the pane
/// it divides, every pane of the window for a full-size split, and in a
/// zoomed window the zoomed pane, which the split unzooms. A size given with
/// `-l`, before or after the target, stays inside the divided pane's cell
/// (`Window::resize_new_split_pane_to`), so it resizes no other pane. `None`
/// for a missing session, window or pane, which the split's own lookup
/// reports.
pub(super) fn split_resized_panes(
    sessions: &ManagedSessionStore,
    target: &SplitWindowTarget,
    full_size: bool,
) -> Option<(u32, Vec<PaneId>)> {
    let session = sessions.session(split_window_session_name(target))?;
    let window_index = split_window_target_window_index(session, target);
    let window = session.window_at(window_index)?;
    if full_size {
        let every_pane = window.panes().iter().map(|pane| pane.id()).collect();
        return Some((window_index, every_pane));
    }
    let pane_index = match target {
        SplitWindowTarget::Session(_) => session.active_pane_index(),
        SplitWindowTarget::Pane(target) => target.pane_index(),
    };
    let mut resized = vec![window.pane(pane_index)?.id()];
    if window.is_zoomed() {
        resized.extend(window.active_pane().map(|pane| pane.id()));
    }
    Some((window_index, resized))
}

pub(super) fn preview_kill_pane(
    sessions: &ManagedSessionStore,
    target: &PaneTarget,
    kill_all_except: bool,
) -> Result<KillPaneOutcome, RmuxError> {
    sessions.preview_kill_pane(
        &WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        target.pane_index(),
        kill_all_except,
    )
}

pub(super) fn split_window_session_name(target: &SplitWindowTarget) -> &SessionName {
    match target {
        SplitWindowTarget::Session(session_name) => session_name,
        SplitWindowTarget::Pane(target) => target.session_name(),
    }
}

fn split_window_target_window_index(
    session: rmux_core::ManagedSessionView<'_>,
    target: &SplitWindowTarget,
) -> u32 {
    match target {
        SplitWindowTarget::Session(_) => session.active_window_index(),
        SplitWindowTarget::Pane(target) => target.window_index(),
    }
}

pub(super) fn split_window_internal_direction(direction: SplitDirection) -> SplitDirection {
    match direction {
        SplitDirection::Horizontal => SplitDirection::Vertical,
        SplitDirection::Vertical => SplitDirection::Horizontal,
    }
}
