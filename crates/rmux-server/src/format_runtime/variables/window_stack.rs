//! The session's window visit stack.

pub(super) fn session_stack(session: rmux_core::ManagedSessionView<'_>) -> String {
    window_stack(session)
        .into_iter()
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// The current window, then the windows this session really visited, most recent
/// first.
///
/// tmux prints `curw->idx` and then every index of `s->lastw` in its own order
/// (`format.c:639-659`); a window that was never visited is in neither. The
/// session keeps that history itself, so nothing here fabricates an order out of
/// window numbers.
fn window_stack(session: rmux_core::ManagedSessionView<'_>) -> Vec<u32> {
    let mut stack = Vec::with_capacity(session.window_visit_stack().len() + 1);
    push_existing_window(&mut stack, session, session.active_window_index());
    for window_index in session.window_visit_stack() {
        push_existing_window(&mut stack, session, *window_index);
    }
    stack
}

fn push_existing_window(
    stack: &mut Vec<u32>,
    session: rmux_core::ManagedSessionView<'_>,
    window_index: u32,
) {
    if session.window_at(window_index).is_some() && !stack.contains(&window_index) {
        stack.push(window_index);
    }
}
