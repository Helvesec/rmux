use super::*;

#[derive(Clone, Copy)]
pub(in crate::handler) struct ControlResizeClient<'a> {
    active_attach: &'a crate::handler::attach_support::ActiveAttachState,
    active_control: &'a crate::handler::ActiveControlState,
    control_pid: u32,
    /// `None` while the client has never announced a size with
    /// `refresh-client -C`; such a client owns no window geometry, exactly as
    /// tmux's `ignore_client_size()` skips it.
    declared_size: Option<TerminalSize>,
    size_sequence: u64,
}

impl<'a> ControlResizeClient<'a> {
    pub(in crate::handler) const fn new(
        active_attach: &'a crate::handler::attach_support::ActiveAttachState,
        active_control: &'a crate::handler::ActiveControlState,
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
    let windows = state
        .sessions
        .session(session_name)
        .filter(|session| session.id() == expected_session_id)
        .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?
        .windows()
        .map(|(index, window)| (index, window.id()))
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut resizes = Vec::new();
    for (index, id) in windows {
        if !seen.insert(id) {
            continue;
        }
        let (_, selected) = control_resize_selection(
            state,
            client,
            session_name,
            expected_session_id,
            index,
            false,
        )?;
        let session = state
            .sessions
            .session(session_name)
            .expect("validated session remains borrowed");
        if let Some(selected) = selected.filter(|selected| !selected.matches_window(session, index))
        {
            resizes.push(crate::pane_terminals::AttachedWindowResize {
                target: WindowTarget::with_window(session_name.clone(), index),
                terminal_size: selected.terminal_size,
                content_size: selected.content_size,
                constrained_manual: selected.constrained_manual,
            });
        }
    }
    state.resize_attached_windows(&resizes)
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
        true,
    )?;
    let resizes = selected_size.is_some_and(|selected_size| selected_size != current_size);
    if selection.is_none() && !resizes {
        return Ok(Vec::new());
    }

    let target = WindowTarget::with_window(session_name.clone(), window_index);
    let (_, refresh_sessions) = state.mutate_switch_window_and_resize_terminal_with_family(
        &target,
        Some(expected_session_id),
        false,
        |mut switch| {
            if let Some(selection) = selection {
                selection.apply_to_window_switch(&mut switch)?;
            }
            if let Some(selected_size) = selected_size {
                selected_size.apply_to_window_switch(&mut switch);
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
    joining_window: bool,
) -> Result<(SelectedWindowSize, Option<SelectedWindowSize>), RmuxError> {
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
        linked_session_statuses(state, session_name, window_index, aggressive_resize);
    let candidates = attached_size_candidates(client.active_attach, &linked_sessions, None, None);
    let window = session.window_at(window_index).expect("validated window");
    let mut control_candidates = control_size_candidates(
        client.active_control,
        &linked_sessions,
        Some(client.control_pid),
        window.id(),
    );
    if let Some(size) = client
        .declared_size
        .filter(|_| joining_window || linked_sessions.contains(session_name, expected_session_id))
    {
        let active = client
            .active_control
            .by_pid
            .get(&client.control_pid)
            .expect("control registration locked by caller");
        let window_limit = active.window_sizes.get(window.id());
        control_candidates.push(ControlSizeCandidate {
            pid: client.control_pid,
            control_id: active.id,
            session_name: session_name.clone(),
            session_id: expected_session_id,
            size: window_limit.unwrap_or(size),
            window_limit,
            sequence: client.size_sequence,
        });
    }
    Ok((
        SelectedWindowSize {
            constrained_manual: policy == AttachedWindowSizePolicy::Manual,
            terminal_size: session.terminal_size(),
            content_size: current_size,
        },
        selected_client_size(
            policy,
            candidates,
            &control_candidates,
            window.manual_size(),
        ),
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
pub(super) fn control_size_candidates(
    active_control: &crate::handler::ActiveControlState,
    linked_sessions: &LinkedSessions,
    excluded_pid: Option<u32>,
    window_id: WindowId,
) -> Vec<ControlSizeCandidate> {
    let mut candidates = active_control
        .by_pid
        .iter()
        .filter(|(pid, active)| {
            Some(**pid) != excluded_pid
                && (active.size_declared || active.window_sizes.declared())
                && !active.flags.ignore_size
                && !active.closing.load(Ordering::Acquire)
                && active
                    .session_name
                    .as_ref()
                    .zip(active.session_id)
                    .is_some_and(|(session_name, session_id)| {
                        linked_sessions.contains(session_name, session_id)
                    })
        })
        .filter_map(|(&pid, active)| {
            let (session_name, session_id) = active.session_name.clone().zip(active.session_id)?;
            Some(ControlSizeCandidate {
                pid,
                control_id: active.id,
                session_name,
                session_id,
                size: active.window_sizes.get(window_id).unwrap_or(TerminalSize {
                    cols: active.client_width,
                    rows: active.client_height,
                }),
                window_limit: active.window_sizes.get(window_id),
                sequence: active.size_sequence,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| (candidate.pid, candidate.control_id));
    candidates
}
