use rmux_core::{BreakPaneOptions, PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget};
use rmux_proto::{
    BreakPaneRequest, BreakPaneResponse, JoinPaneRequest, JoinPaneResponse, PaneTarget, RmuxError,
    SessionName, SwapPaneResponse, WindowTarget,
};

use super::super::{session_not_found, HandlerState};
use super::{join_pane_internal_direction, pane_id_for_target, pane_index_for_id};

impl HandlerState {
    pub(super) fn swap_pane_across_sessions(
        &mut self,
        source: PaneTarget,
        target: PaneTarget,
        detached: bool,
        preserve_zoom: bool,
    ) -> Result<SwapPaneResponse, RmuxError> {
        let source_session_name = source.session_name().clone();
        let target_session_name = target.session_name().clone();
        let previous_source_session = self
            .sessions
            .session(&source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let previous_target_session = self
            .sessions
            .session(&target_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&target_session_name))?;
        let source_pane_id = pane_id_for_target(&previous_source_session, &source)?;
        let target_pane_id = pane_id_for_target(&previous_target_session, &target)?;
        self.ensure_panes_exist(&source_session_name, &[source_pane_id])?;
        self.ensure_panes_exist(&target_session_name, &[target_pane_id])?;

        let mut source_session = self
            .sessions
            .remove_session(&source_session_name)
            .map_err(|_| session_not_found(&source_session_name))?;
        let mutation_result = {
            let target_session = self
                .sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?;
            source_session.swap_panes_with_session(
                SessionPaneTarget::from(&source),
                target_session,
                SessionPaneTarget::from(&target),
                PaneSwapOptions::new(detached, preserve_zoom),
            )
        };
        self.sessions.insert_existing_session(source_session)?;
        if let Err(error) = mutation_result {
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }

        if let Err(error) = self.terminals.swap_panes_between_sessions(
            &source_session_name,
            &[source_pane_id],
            &target_session_name,
            &[target_pane_id],
        ) {
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }
        if let Err(error) = self.swap_pane_outputs_between_sessions(
            &source_session_name,
            &[source_pane_id],
            &target_session_name,
            &[target_pane_id],
        ) {
            self.terminals.swap_panes_between_sessions(
                &source_session_name,
                &[target_pane_id],
                &target_session_name,
                &[source_pane_id],
            )?;
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }

        if let Err(error) = resize_two_sessions(self, &source_session_name, &target_session_name) {
            self.terminals.swap_panes_between_sessions(
                &source_session_name,
                &[target_pane_id],
                &target_session_name,
                &[source_pane_id],
            )?;
            self.swap_pane_outputs_between_sessions(
                &source_session_name,
                &[target_pane_id],
                &target_session_name,
                &[source_pane_id],
            )?;
            restore_two_sessions_after_resize_error(
                self,
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
                &error,
            )?;
            return Err(error);
        }

        self.synchronize_session_group_from(&source_session_name)?;
        if source_session_name != target_session_name {
            self.synchronize_session_group_from(&target_session_name)?;
        }
        self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
        if source_session_name != target_session_name {
            self.sync_pane_lifecycle_dimensions_for_session(&target_session_name);
        }

        Ok(SwapPaneResponse { source, target })
    }

    pub(super) fn join_pane_across_sessions(
        &mut self,
        request: JoinPaneRequest,
    ) -> Result<JoinPaneResponse, RmuxError> {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let previous_source_session = self
            .sessions
            .session(&source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let previous_target_session = self
            .sessions
            .session(&target_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&target_session_name))?;
        let source_pane_id = pane_id_for_target(&previous_source_session, &request.source)?;
        self.ensure_panes_exist(&source_session_name, &[source_pane_id])?;

        let mut source_session = self
            .sessions
            .remove_session(&source_session_name)
            .map_err(|_| session_not_found(&source_session_name))?;
        let mutation_result = {
            let target_session = self
                .sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?;
            let direction = join_pane_internal_direction(request.direction);
            target_session.join_pane_from_session(
                SessionPaneTarget::from(&request.target),
                &mut source_session,
                SessionPaneTarget::from(&request.source),
                PaneJoinOptions::new(
                    direction,
                    request.detached,
                    request.before,
                    request.full_size,
                    request.size,
                ),
            )
        };
        self.sessions.insert_existing_session(source_session)?;
        if let Err(error) = mutation_result {
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }

        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_session_name,
            &target_session_name,
            &[source_pane_id],
        ) {
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_session_name,
            &target_session_name,
            &[source_pane_id],
        ) {
            self.terminals.move_panes_between_sessions(
                &target_session_name,
                &source_session_name,
                &[source_pane_id],
            )?;
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }

        if let Err(error) = resize_two_sessions(self, &source_session_name, &target_session_name) {
            self.terminals.move_panes_between_sessions(
                &target_session_name,
                &source_session_name,
                &[source_pane_id],
            )?;
            self.move_pane_outputs_between_sessions(
                &target_session_name,
                &source_session_name,
                &[source_pane_id],
            )?;
            restore_two_sessions_after_resize_error(
                self,
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
                &error,
            )?;
            return Err(error);
        }

        self.synchronize_session_group_from(&source_session_name)?;
        if source_session_name != target_session_name {
            self.synchronize_session_group_from(&target_session_name)?;
        }
        self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
        if source_session_name != target_session_name {
            self.sync_pane_lifecycle_dimensions_for_session(&target_session_name);
        }

        let moved_index = self
            .sessions
            .session(&target_session_name)
            .and_then(|session| {
                pane_index_for_id(session, request.target.window_index(), source_pane_id)
            })
            .ok_or_else(|| {
                RmuxError::Server("moved pane disappeared after cross-session join-pane".to_owned())
            })?;

        Ok(JoinPaneResponse {
            target: PaneTarget::with_window(
                target_session_name,
                request.target.window_index(),
                moved_index,
            ),
        })
    }

    pub(super) fn break_pane_across_sessions(
        &mut self,
        request: BreakPaneRequest,
        destination_session_name: SessionName,
    ) -> Result<BreakPaneResponse, RmuxError> {
        let source_session_name = request.source.session_name().clone();
        let previous_source_session = self
            .sessions
            .session(&source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let previous_destination_session = self
            .sessions
            .session(&destination_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&destination_session_name))?;
        let source_pane_id = pane_id_for_target(&previous_source_session, &request.source)?;
        self.ensure_panes_exist(&source_session_name, &[source_pane_id])?;

        let mut source_session = self
            .sessions
            .remove_session(&source_session_name)
            .map_err(|_| session_not_found(&source_session_name))?;
        let destination_index = {
            let destination_session = self
                .sessions
                .session_mut(&destination_session_name)
                .ok_or_else(|| session_not_found(&destination_session_name))?;
            source_session.break_pane_to_session(
                SessionPaneTarget::from(&request.source),
                destination_session,
                BreakPaneOptions::new(
                    request.target.as_ref().map(WindowTarget::window_index),
                    request.name.clone(),
                    request.detached,
                    request.after,
                    request.before,
                ),
            )
        };
        self.sessions.insert_existing_session(source_session)?;
        let destination_index = match destination_index {
            Ok(destination_index) => destination_index,
            Err(error) => {
                self.restore_cross_session_snapshots(
                    &source_session_name,
                    previous_source_session,
                    &destination_session_name,
                    previous_destination_session,
                )?;
                return Err(error);
            }
        };

        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_session_name,
            &destination_session_name,
            &[source_pane_id],
        ) {
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &destination_session_name,
                previous_destination_session,
            )?;
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_session_name,
            &destination_session_name,
            &[source_pane_id],
        ) {
            self.terminals.move_panes_between_sessions(
                &destination_session_name,
                &source_session_name,
                &[source_pane_id],
            )?;
            self.restore_cross_session_snapshots(
                &source_session_name,
                previous_source_session,
                &destination_session_name,
                previous_destination_session,
            )?;
            return Err(error);
        }

        if let Err(error) =
            resize_two_sessions(self, &source_session_name, &destination_session_name)
        {
            self.terminals.move_panes_between_sessions(
                &destination_session_name,
                &source_session_name,
                &[source_pane_id],
            )?;
            self.move_pane_outputs_between_sessions(
                &destination_session_name,
                &source_session_name,
                &[source_pane_id],
            )?;
            restore_two_sessions_after_resize_error(
                self,
                &source_session_name,
                previous_source_session,
                &destination_session_name,
                previous_destination_session,
                &error,
            )?;
            return Err(error);
        }

        self.synchronize_session_group_from(&source_session_name)?;
        if source_session_name != destination_session_name {
            self.synchronize_session_group_from(&destination_session_name)?;
        }
        self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
        if source_session_name != destination_session_name {
            self.sync_pane_lifecycle_dimensions_for_session(&destination_session_name);
        }

        Ok(BreakPaneResponse {
            target: PaneTarget::with_window(destination_session_name, destination_index, 0),
            output: None,
        })
    }

    fn restore_cross_session_snapshots(
        &mut self,
        source_session_name: &SessionName,
        previous_source_session: Session,
        target_session_name: &SessionName,
        previous_target_session: Session,
    ) -> Result<(), RmuxError> {
        self.replace_session(source_session_name, previous_source_session)?;
        self.replace_session(target_session_name, previous_target_session)
    }
}

fn resize_two_sessions(
    state: &mut HandlerState,
    source_session_name: &SessionName,
    target_session_name: &SessionName,
) -> Result<(), RmuxError> {
    state.resize_terminals(source_session_name)?;
    state.resize_terminals(target_session_name)
}

fn restore_two_sessions_after_resize_error(
    state: &mut HandlerState,
    source_session_name: &SessionName,
    previous_source_session: Session,
    target_session_name: &SessionName,
    previous_target_session: Session,
    source_error: &RmuxError,
) -> Result<(), RmuxError> {
    state.replace_session(source_session_name, previous_source_session)?;
    state.replace_session(target_session_name, previous_target_session)?;
    resize_two_sessions(state, source_session_name, target_session_name).map_err(
        |rollback_error| {
            RmuxError::Server(format!(
                "failed to roll back sessions {source_session_name} and {target_session_name} after {source_error}: {rollback_error}"
            ))
        },
    )
}
