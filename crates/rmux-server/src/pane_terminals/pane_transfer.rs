use rmux_core::PaneId;
use rmux_proto::{
    BreakPaneRequest, BreakPaneResponse, JoinPaneRequest, JoinPaneResponse, LastPaneResponse,
    MovePaneRequest, MovePaneResponse, PaneTarget, RmuxError, SplitDirection, SwapPaneDirection,
    SwapPaneRequest, SwapPaneResponse, WindowTarget,
};

use super::{session_mutation::PaneTransferGeometryContext, session_not_found, HandlerState};

mod cross_session;
mod grouped;
mod linked_last;
mod window_metadata;

impl HandlerState {
    pub(crate) fn last_pane(
        &mut self,
        target: WindowTarget,
        preserve_zoom: bool,
        input_disabled: Option<bool>,
    ) -> Result<(LastPaneResponse, Vec<rmux_proto::SessionName>), RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        // tmux 3.7b (cmd-select-pane.c): -e or -d toggles the last pane's
        // input and selects nothing; only a plain last-pane selects it.
        if let Some(disabled) = input_disabled {
            let pane_index = self
                .sessions
                .resolve_window(&rmux_proto::Target::Window(target))?
                .last_pane_or_two_pane_fallback()
                .ok_or_else(|| RmuxError::Server("no last pane".to_owned()))?;
            let response_target = PaneTarget::with_window(session_name, window_index, pane_index);
            self.set_pane_input_disabled(&response_target, disabled)?;
            return Ok((
                LastPaneResponse {
                    target: response_target,
                },
                Vec::new(),
            ));
        }
        let (pane_index, synchronized_sessions) = self
            .mutate_window_and_resize_terminal_with_family(&target, |selection| {
                selection.select_last(preserve_zoom)
            })?;
        let response_target = PaneTarget::with_window(session_name, window_index, pane_index);

        Ok((
            LastPaneResponse {
                target: response_target,
            },
            synchronized_sessions,
        ))
    }

    pub(crate) fn swap_pane(
        &mut self,
        request: SwapPaneRequest,
    ) -> Result<SwapPaneResponse, RmuxError> {
        let (source, target) = resolve_swap_targets(&self.sessions, &request)?;
        // Once the swap commits, both windows' linked families are resized
        // and refreshed as a join or move refreshes them: a poisoned pane the
        // swap does not name reads as formats observe it. A pane it names
        // fails fast before anything changes, and a refused floating swap
        // below may already unzoom.
        self.ensure_pane_available(&source)?;
        self.ensure_pane_available(&target)?;
        self.reject_floating_swap(&source, &target)?;
        let response = if pane_targets_share_window_identity(&self.sessions, &source, &target) {
            let normalized_target = PaneTarget::with_window(
                source.session_name().clone(),
                source.window_index(),
                target.pane_index(),
            );
            self.swap_pane_within_group(
                source.clone(),
                normalized_target,
                request.detached,
                request.preserve_zoom,
            )?;
            SwapPaneResponse { source, target }
        } else if sessions_share_grouped_window_state(
            &self.sessions,
            source.session_name(),
            target.session_name(),
        ) {
            self.swap_pane_within_group(source, target, request.detached, request.preserve_zoom)?
        } else {
            self.swap_pane_across_sessions(source, target, request.detached, request.preserve_zoom)?
        };
        self.refresh_all_transcript_cursor_styles();
        self.refresh_all_transcript_scroll_on_clear();
        self.refresh_all_transcript_allow_rename();
        Ok(response)
    }

    fn reject_floating_swap(
        &mut self,
        source: &PaneTarget,
        target: &PaneTarget,
    ) -> Result<(), RmuxError> {
        let endpoint = |target: &PaneTarget| {
            let session = self
                .sessions
                .session(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?;
            let pane_id = pane_id_for_target(session, target)?;
            let window = session
                .window_at(target.window_index())
                .expect("resolved pane belongs to a window");
            Ok::<_, RmuxError>((
                window.id(),
                pane_id,
                window.floating_stack().placement(pane_id).is_some(),
            ))
        };
        let (source_window, source_pane, source_floating) = endpoint(source)?;
        let (target_window, target_pane, target_floating) = endpoint(target)?;
        if source_pane == target_pane || !(source_floating || target_floating) {
            return Ok(());
        }

        // tmux 3.7b unzooms before refusing distinct floating panes, even with
        // -Z. Commit only that measured effect before the transfer snapshots;
        // keep their rollback guarantees for all other mutations and errors.
        for pane in [
            Some(target),
            (source_window != target_window).then_some(source),
        ]
        .into_iter()
        .flatten()
        {
            let window =
                WindowTarget::with_window(pane.session_name().clone(), pane.window_index());
            self.mutate_store_and_resize_window_terminal_with_family_if(
                window.session_name(),
                window.window_index(),
                |store| {
                    let mut layout = store.window_layout_context_mut(&window)?;
                    let zoomed = layout.is_zoomed();
                    if zoomed {
                        layout.toggle_zoom(layout.active_pane_index())?;
                    }
                    Ok(zoomed)
                },
                |zoomed| *zoomed,
            )
            .map_err(RmuxError::from)?;
        }
        Err(RmuxError::Server("cannot swap floating panes".to_owned()))
    }

    pub(crate) fn join_pane(
        &mut self,
        request: JoinPaneRequest,
    ) -> Result<JoinPaneResponse, RmuxError> {
        let geometry_context = PaneTransferGeometryContext::new(&request.source, &request.target);
        let response = self
            .mutate_join_or_move_and_record_window_geometry_changes(geometry_context, |state| {
                state.join_pane_unrecorded(request)
            })?;
        self.refresh_all_transcript_cursor_styles();
        self.refresh_all_transcript_scroll_on_clear();
        self.refresh_all_transcript_allow_rename();
        Ok(response)
    }

    fn join_pane_unrecorded(
        &mut self,
        request: JoinPaneRequest,
    ) -> Result<JoinPaneResponse, RmuxError> {
        if request.source == request.target {
            return Err(RmuxError::Server(
                "source and target panes must be different".to_owned(),
            ));
        }
        // join-pane and move-pane name both panes and resize both windows. A
        // poisoned named pane fails fast before anything moves, as
        // swap-pane's and break-pane's do.
        self.ensure_pane_available(&request.source)?;
        self.ensure_pane_available(&request.target)?;
        if pane_targets_share_window_identity(&self.sessions, &request.source, &request.target) {
            let mut normalized_request = request;
            normalized_request.source = PaneTarget::with_window(
                normalized_request.target.session_name().clone(),
                normalized_request.target.window_index(),
                normalized_request.source.pane_index(),
            );
            return self.join_pane_within_group(normalized_request);
        }
        if sessions_share_grouped_window_state(
            &self.sessions,
            request.source.session_name(),
            request.target.session_name(),
        ) {
            return self.join_pane_within_group(request);
        }

        self.join_pane_across_sessions(request)
    }

    pub(crate) fn move_pane(
        &mut self,
        request: MovePaneRequest,
    ) -> Result<MovePaneResponse, RmuxError> {
        let geometry_context = PaneTransferGeometryContext::new(&request.source, &request.target);
        let response = self.mutate_join_or_move_and_record_window_geometry_changes(
            geometry_context,
            |state| {
                let response = state.join_pane_unrecorded(JoinPaneRequest {
                    source: request.source,
                    target: request.target,
                    direction: request.direction,
                    detached: request.detached,
                    before: request.before,
                    full_size: request.full_size,
                    size: request.size,
                })?;
                Ok::<_, RmuxError>(MovePaneResponse {
                    target: response.target,
                })
            },
        )?;
        self.refresh_all_transcript_cursor_styles();
        self.refresh_all_transcript_scroll_on_clear();
        self.refresh_all_transcript_allow_rename();
        Ok(response)
    }

    pub(crate) fn break_pane(
        &mut self,
        mut request: BreakPaneRequest,
    ) -> Result<BreakPaneResponse, RmuxError> {
        let explicit_name = request.name.is_some();
        let destination_session_name = request.target.as_ref().map_or_else(
            || request.source.session_name().clone(),
            |target| target.session_name().clone(),
        );
        if request.target.is_none() && !(request.after || request.before) {
            let destination_index = self.first_available_window_index(&destination_session_name)?;
            request.target = Some(WindowTarget::with_window(
                destination_session_name.clone(),
                destination_index,
            ));
        }
        let shares_grouped_window_state = sessions_share_grouped_window_state(
            &self.sessions,
            request.source.session_name(),
            &destination_session_name,
        );

        if request.source.session_name() != &destination_session_name
            && shares_grouped_window_state
            && pane_is_only_pane_in_window(&self.sessions, &request.source)
        {
            return Err(RmuxError::Server("sessions are grouped".to_owned()));
        }

        let creates_window = self
            .sessions
            .session(request.source.session_name())
            .and_then(|session| session.window_at(request.source.window_index()))
            .is_some_and(|window| window.pane_count() > 1);
        // The broken pane's window is resized wherever it lands: a poisoned
        // source fails fast before any window changes.
        self.ensure_pane_available(&request.source)?;
        let initial_name = if !explicit_name
            && creates_window
            && !crate::automatic_rename::automatic_rename_enabled_for_new_window(
                &self.options,
                &destination_session_name,
            ) {
            self.pane_runtime_window_name_in_window(
                request.source.session_name(),
                request.source.window_index(),
                request.source.pane_index(),
            )?
            .filter(|name| !name.is_empty())
        } else {
            None
        };
        if let Some(name) = initial_name.as_ref() {
            // Feed the stored runtime name into the core creation transaction.
            // The original request remains implicit for marker semantics.
            request.name = Some(name.clone());
        }

        let response = if shares_grouped_window_state {
            self.break_pane_within_group(request, destination_session_name)
        } else {
            self.break_pane_across_sessions(request, destination_session_name)
        }?;
        if explicit_name {
            // `-n` pins the destination name. A moved window can carry the
            // source's auto-name marker, which would otherwise override the
            // explicit name on the next pane activity callback.
            self.clear_auto_named_window_family(
                response.target.session_name(),
                response.target.window_index(),
            );
        } else if initial_name.is_some() {
            self.mark_auto_named_window(
                response.target.session_name(),
                response.target.window_index(),
            );
        }
        self.refresh_all_transcript_cursor_styles();
        self.refresh_all_transcript_scroll_on_clear();
        self.refresh_all_transcript_allow_rename();
        Ok(response)
    }
}

fn pane_targets_share_window_identity(
    sessions: &rmux_core::ManagedSessionStore,
    first: &PaneTarget,
    second: &PaneTarget,
) -> bool {
    let first_window_id = sessions
        .session(first.session_name())
        .and_then(|session| session.window_at(first.window_index()))
        .map(rmux_core::Window::id);
    let second_window_id = sessions
        .session(second.session_name())
        .and_then(|session| session.window_at(second.window_index()))
        .map(rmux_core::Window::id);
    first_window_id.is_some() && first_window_id == second_window_id
}

fn pane_is_only_pane_in_window(
    sessions: &rmux_core::ManagedSessionStore,
    target: &PaneTarget,
) -> bool {
    let Some(session) = sessions.session(target.session_name()) else {
        return false;
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return false;
    };

    window.panes().len() == 1
        && session
            .pane_id_in_window(target.window_index(), target.pane_index())
            .is_some()
}

fn sessions_share_grouped_window_state(
    sessions: &rmux_core::ManagedSessionStore,
    first: &rmux_proto::SessionName,
    second: &rmux_proto::SessionName,
) -> bool {
    if first == second {
        return true;
    }

    let Some(first_group) = sessions.session_group_name(first) else {
        return false;
    };
    sessions.session_group_name(second) == Some(first_group)
}

fn join_pane_internal_direction(direction: SplitDirection) -> SplitDirection {
    match direction {
        SplitDirection::Horizontal => SplitDirection::Vertical,
        SplitDirection::Vertical => SplitDirection::Horizontal,
    }
}

fn pane_id_for_target(
    session: rmux_core::ManagedSessionView<'_>,
    target: &PaneTarget,
) -> Result<PaneId, RmuxError> {
    session
        .pane_id_in_window(target.window_index(), target.pane_index())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })
}

fn pane_index_for_id(
    session: rmux_core::ManagedSessionView<'_>,
    window_index: u32,
    pane_id: PaneId,
) -> Option<u32> {
    session.window_at(window_index).and_then(|window| {
        window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(|pane| pane.index())
    })
}

fn pane_target_for_id(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<PaneTarget, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let window_index = session.window_index_for_pane_id(pane_id).ok_or_else(|| {
        RmuxError::Server("moved pane disappeared after pane transfer".to_owned())
    })?;
    let pane_index = pane_index_for_id(session, window_index, pane_id).ok_or_else(|| {
        RmuxError::Server("moved pane index disappeared after pane transfer".to_owned())
    })?;
    Ok(PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane_index,
    ))
}

fn resolve_swap_targets(
    sessions: &rmux_core::ManagedSessionStore,
    request: &SwapPaneRequest,
) -> Result<(PaneTarget, PaneTarget), RmuxError> {
    if let Some(direction) = request.direction {
        let anchor = &request.target;
        let session = sessions
            .session(anchor.session_name())
            .ok_or_else(|| session_not_found(anchor.session_name()))?;
        let window = session.window_at(anchor.window_index()).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{}:{}", anchor.session_name(), anchor.window_index()),
                "window index does not exist in session",
            )
        })?;
        let anchor_position = window
            .panes()
            .iter()
            .position(|pane| pane.index() == anchor.pane_index())
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    anchor.to_string(),
                    "pane index does not exist in session",
                )
            })?;
        let pane_count = window.pane_count();
        let source_position = match direction {
            SwapPaneDirection::Down => (anchor_position + 1) % pane_count,
            SwapPaneDirection::Up => (anchor_position + pane_count - 1) % pane_count,
        };
        let source_pane_index = window
            .panes()
            .get(source_position)
            .expect("resolved pane position must exist")
            .index();

        return Ok((
            PaneTarget::with_window(
                anchor.session_name().clone(),
                anchor.window_index(),
                source_pane_index,
            ),
            anchor.clone(),
        ));
    }

    Ok((request.source.clone(), request.target.clone()))
}
