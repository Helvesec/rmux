use rmux_proto::{
    LinkWindowRequest, LinkWindowResponse, RmuxError, UnlinkWindowResponse, WindowTarget,
};

use super::RemovedWindowHookContext;
use super::{link_window_destination_index, session_not_found, window_pane_ids, HandlerState};
use crate::pane_terminals::{LinkedWindowResult, UnlinkedWindowResult};

impl HandlerState {
    pub(crate) fn link_window(
        &mut self,
        request: LinkWindowRequest,
    ) -> Result<LinkedWindowResult, RmuxError> {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let target_window_index = {
            let session = self
                .sessions
                .session(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?;
            link_window_destination_index(
                session,
                request.target.window_index(),
                request.after,
                request.before,
            )?
        };

        if !(request.after || request.before)
            && source_session_name == target_session_name
            && request.source.window_index() == target_window_index
        {
            return Err(RmuxError::Server(format!(
                "same index: {target_window_index}"
            )));
        }

        let source_window = self
            .sessions
            .session(&source_session_name)
            .and_then(|session| session.window_at(request.source.window_index()))
            .cloned()
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    request.source.to_string(),
                    "window index does not exist in session",
                )
            })?;
        let source_pane_ids = source_window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>();
        let source_runtime_session = self
            .runtime_session_name_for_window(&source_session_name, request.source.window_index());
        self.terminals
            .ensure_panes_exist(&source_runtime_session, &source_pane_ids)?;

        let previous_target_session = self
            .sessions
            .session(&target_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&target_session_name))?;
        let replaced_target = if request.after || request.before {
            None
        } else {
            previous_target_session
                .window_at(target_window_index)
                .map(|_window| {
                    (
                        window_pane_ids(
                            &previous_target_session,
                            &target_session_name,
                            target_window_index,
                        )
                        .expect("window ids were already validated"),
                        self.window_link_count(&target_session_name, target_window_index),
                        self.runtime_session_name_for_window(
                            &target_session_name,
                            target_window_index,
                        ),
                    )
                })
        };
        let replaced_pane_ids = replaced_target
            .as_ref()
            .map(|(pane_ids, _, _)| pane_ids.clone())
            .unwrap_or_default();

        let adjusted_source_window_index = if source_session_name == target_session_name
            && (request.after || request.before)
            && request.source.window_index() >= target_window_index
        {
            request.source.window_index().saturating_add(1)
        } else {
            request.source.window_index()
        };

        let inserted = {
            let session = self
                .sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?;
            if request.after || request.before {
                session.make_room_for_window(target_window_index)?;
            }
            session.link_window(
                target_window_index,
                source_window,
                request.kill_destination,
                !request.detached,
            )?
        };

        if let (Some((pane_ids, link_count, runtime_session_name)), Some(_)) =
            (replaced_target, inserted)
        {
            let _ = self.options.remove_window(&WindowTarget::with_window(
                target_session_name.clone(),
                target_window_index,
            ));
            let _ = self.hooks.remove_window(&WindowTarget::with_window(
                target_session_name.clone(),
                target_window_index,
            ));
            self.clear_auto_named_window(&target_session_name, target_window_index);
            if link_count > 1 {
                let _ = self.detach_window_link_slot(&target_session_name, target_window_index);
            } else {
                for pane_id in &pane_ids {
                    if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, *pane_id) {
                        pipe.stop();
                    }
                }
                let _ = self
                    .terminals
                    .remove_pane_batch(&runtime_session_name, &pane_ids)?;
                let mut removed_outputs =
                    self.remove_pane_outputs(&runtime_session_name, &pane_ids);
                removed_outputs.abort_output_readers();
                self.remove_pane_lifecycles(&pane_ids);
            }
        }

        self.attach_window_link_slot(
            &source_session_name,
            adjusted_source_window_index,
            &target_session_name,
            target_window_index,
        );
        self.synchronize_linked_window_options_from_slot(
            &source_session_name,
            adjusted_source_window_index,
        );
        self.synchronize_linked_window_from_slot(
            &source_session_name,
            adjusted_source_window_index,
        )?;
        self.synchronize_session_group_from(&target_session_name)?;
        if source_session_name != target_session_name {
            self.synchronize_session_group_from(&source_session_name)?;
        }
        if let Some(source_session) = self.sessions.session(&source_session_name).cloned() {
            self.synchronize_pane_alias_options_from_session(&source_session)?;
        }
        self.sync_pane_lifecycle_dimensions_for_session(&target_session_name);
        if source_session_name != target_session_name {
            self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
        }

        Ok(LinkedWindowResult {
            response: LinkWindowResponse {
                target: WindowTarget::with_window(target_session_name, target_window_index),
            },
            removed_pane_ids: self.pane_ids_no_longer_referenced(replaced_pane_ids),
        })
    }

    pub(crate) fn unlink_window(
        &mut self,
        target: WindowTarget,
        kill_if_last: bool,
    ) -> Result<UnlinkedWindowResult, RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let removed_window = self
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(window_index))
            .map(|window| RemovedWindowHookContext {
                target: target.clone(),
                window_id: window.id().as_u32(),
                window_name: window.name().unwrap_or_default().to_owned(),
            })
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
        let link_count = self.window_link_count(&session_name, window_index);
        if link_count == 1 {
            if !kill_if_last {
                return Err(RmuxError::Message(
                    "window only linked to one session".to_owned(),
                ));
            }
            let killed = self.kill_window(target, false)?;
            return Ok(UnlinkedWindowResult {
                response: UnlinkWindowResponse {
                    target: killed.response.target,
                },
                removed_window,
                removed_pane_ids: killed.removed_pane_ids,
            });
        }

        let active_window = {
            let session = self
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            let target_was_active = session.active_window_index() == window_index;
            let previous_last = session.last_window_index();
            let _removed_window = session.remove_window(window_index)?;
            if target_was_active && previous_last == Some(session.active_window_index()) {
                session.restore_last_window_after_active_unlink();
            }
            session.active_window_index()
        };

        let _ = self.options.remove_window(&target);
        let _ = self.hooks.remove_window(&target);
        self.clear_auto_named_window(&session_name, window_index);
        let _ = self.detach_window_link_slot(&session_name, window_index);
        self.synchronize_session_group_from(&session_name)?;

        Ok(UnlinkedWindowResult {
            response: UnlinkWindowResponse {
                target: WindowTarget::with_window(session_name, active_window),
            },
            removed_window,
            removed_pane_ids: Vec::new(),
        })
    }
}
