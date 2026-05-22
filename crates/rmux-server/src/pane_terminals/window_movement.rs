use std::collections::HashMap;

use rmux_proto::{
    MoveWindowRequest, MoveWindowResponse, MoveWindowTarget, RmuxError, RotateWindowDirection,
    RotateWindowResponse, SwapWindowResponse, WindowTarget,
};

use super::{
    ensure_session_panes_exist, request_target_string, session_not_found, window_pane_ids,
    HandlerState,
};

#[path = "window_movement/cross_session.rs"]
mod cross_session;

impl HandlerState {
    pub(crate) fn move_window(
        &mut self,
        request: MoveWindowRequest,
    ) -> Result<MoveWindowResponse, RmuxError> {
        if request.renumber {
            return self.reindex_windows(request);
        }

        let source = request
            .source
            .ok_or_else(|| RmuxError::Server("move-window requires a source window".to_owned()))?;
        let MoveWindowTarget::Window(target) = request.target else {
            return Err(RmuxError::invalid_target(
                source.session_name().to_string(),
                "move-window requires a destination window target",
            ));
        };

        if source.session_name() == target.session_name() {
            return self.move_window_within_session(
                source,
                target.window_index(),
                request.kill_destination,
                request.detached,
            );
        }

        self.move_window_across_sessions(source, target, request.kill_destination, request.detached)
    }

    pub(crate) fn swap_window(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        detached: bool,
    ) -> Result<SwapWindowResponse, RmuxError> {
        // tmux cmd-swap-window.c:59-65: reject swaps between different sessions
        // in the same session group.
        if source.session_name() != target.session_name() {
            let sg_src = self.sessions.session_group_name(source.session_name());
            let sg_dst = self.sessions.session_group_name(target.session_name());
            if let (Some(sg_src), Some(sg_dst)) = (sg_src, sg_dst)
                && sg_src == sg_dst {
                    return Err(RmuxError::Server(
                        "can't move window, sessions are grouped".to_owned(),
                    ));
                }
        }

        if source.session_name() == target.session_name() {
            let session_name = source.session_name().clone();
            let previous_session = self
                .sessions
                .session(&session_name)
                .cloned()
                .ok_or_else(|| session_not_found(&session_name))?;
            ensure_session_panes_exist(self, &session_name, &previous_session)?;

            {
                let session = self
                    .sessions
                    .session_mut(&session_name)
                    .ok_or_else(|| session_not_found(&session_name))?;
                session.swap_windows(source.window_index(), target.window_index())?;
                // tmux preserves the current winlink unless -d is passed. With
                // -d, it selects the destination winlink after swapping.
                if detached {
                    session.select_window(target.window_index())?;
                }
            }
            self.swap_window_link_slots(
                &session_name,
                source.window_index(),
                target.window_index(),
            );
            self.swap_auto_named_window_slots(
                &session_name,
                source.window_index(),
                &session_name,
                target.window_index(),
            );

            if let Err(error) = self.resize_terminals(&session_name) {
                self.swap_auto_named_window_slots(
                    &session_name,
                    source.window_index(),
                    &session_name,
                    target.window_index(),
                );
                self.swap_window_link_slots(
                    &session_name,
                    source.window_index(),
                    target.window_index(),
                );
                self.restore_session_after_resize_error(&session_name, previous_session, &error)?;
                return Err(error);
            }
            self.synchronize_session_group_from(&session_name)?;

            return Ok(SwapWindowResponse { source, target });
        }

        self.swap_window_across_sessions(source, target, detached)
    }

    pub(crate) fn rotate_window(
        &mut self,
        target: WindowTarget,
        direction: RotateWindowDirection,
        restore_zoom: bool,
    ) -> Result<RotateWindowResponse, RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();

        self.mutate_session_and_resize_terminals(&session_name, |session| {
            if restore_zoom {
                session.rotate_window_with_zoom(window_index, direction, true)?;
            } else {
                session.rotate_window(window_index, direction)?;
            }
            Ok(RotateWindowResponse { target })
        })
    }

    fn reindex_windows(
        &mut self,
        request: MoveWindowRequest,
    ) -> Result<MoveWindowResponse, RmuxError> {
        if request.source.is_some() {
            return Err(RmuxError::Server(
                "move-window -r does not accept a source window".to_owned(),
            ));
        }

        let MoveWindowTarget::Session(session_name) = request.target else {
            return Err(RmuxError::invalid_target(
                request_target_string(&request.target),
                "move-window -r requires a session target",
            ));
        };

        self.mutate_session_and_resize_terminals(&session_name, |session| {
            session.reindex_windows()?;
            Ok(MoveWindowResponse {
                session_name: session_name.clone(),
                target: None,
            })
        })
    }

    fn move_window_within_session(
        &mut self,
        source: WindowTarget,
        destination_index: u32,
        kill_destination: bool,
        detached: bool,
    ) -> Result<MoveWindowResponse, RmuxError> {
        let session_name = source.session_name().clone();
        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        ensure_session_panes_exist(self, &session_name, &previous_session)?;
        let removed_pane_ids = if kill_destination && source.window_index() != destination_index {
            previous_session
                .window_at(destination_index)
                .map(|_| window_pane_ids(&previous_session, &session_name, destination_index))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let should_select_destination = !detached
            || (kill_destination && previous_session.active_window_index() == destination_index);

        {
            let session = self
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            session.move_window(
                source.window_index(),
                destination_index,
                kill_destination,
                should_select_destination,
            )?;
        }

        let removed_terminals = if removed_pane_ids.is_empty() {
            HashMap::new()
        } else {
            match self
                .terminals
                .remove_pane_batch(&session_name, &removed_pane_ids)
            {
                Ok(removed_terminals) => removed_terminals,
                Err(error) => {
                    self.restore_session_after_resize_error(
                        &session_name,
                        previous_session.clone(),
                        &error,
                    )?;
                    return Err(error);
                }
            }
        };
        let removed_outputs = self.remove_pane_outputs(&session_name, &removed_pane_ids);

        if let Err(error) = self.resize_terminals(&session_name) {
            self.replace_session(&session_name, previous_session)?;
            if !removed_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&session_name, removed_terminals)?;
            }
            self.insert_existing_pane_outputs(&session_name, removed_outputs);
            self.resize_terminals(&session_name)
                .map_err(|rollback_error| {
                    RmuxError::Server(format!(
                    "failed to roll back session {session_name} after {error}: {rollback_error}"
                ))
                })?;
            return Err(error);
        }
        self.synchronize_session_group_from(&session_name)?;
        self.remove_pane_lifecycles(&removed_pane_ids);
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        Ok(MoveWindowResponse {
            session_name: session_name.clone(),
            target: Some(WindowTarget::with_window(session_name, destination_index)),
        })
    }
}
