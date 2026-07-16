use std::collections::BTreeMap;

use rmux_proto::{
    LinkWindowRequest, LinkWindowResponse, PaneTarget, RmuxError, UnlinkWindowResponse,
    WindowTarget,
};

use super::RemovedWindowHookContext;
use super::{
    link_window_destination_index, session_not_found, window_pane_ids, HandlerState,
    SessionTransferSnapshot,
};
use crate::pane_terminals::{LinkedWindowResult, UnlinkedWindowResult};

impl HandlerState {
    #[cfg(test)]
    pub(crate) fn fail_next_link_window_after_attach_for_test(&mut self) {
        self.fail_link_window_after_attach = true;
    }

    pub(crate) fn link_window(
        &mut self,
        request: LinkWindowRequest,
    ) -> Result<LinkedWindowResult, RmuxError> {
        self.link_window_with_existing_target_index_map(request, None)
    }

    pub(super) fn link_window_with_existing_target_index_map(
        &mut self,
        request: LinkWindowRequest,
        existing_target_index_map: Option<&BTreeMap<u32, u32>>,
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
        if !(request.after || request.before)
            && self
                .sessions
                .session(&target_session_name)
                .and_then(|session| session.window_at(target_window_index))
                .is_some_and(|target_window| target_window.id() == source_window.id())
        {
            return Err(RmuxError::Server(format!(
                "same window: {}",
                source_window.id()
            )));
        }
        let source_pane_ids = source_window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>();
        let source_pane_indices = source_window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>();
        let source_runtime_session = self
            .runtime_session_name_for_window(&source_session_name, request.source.window_index());
        self.terminals
            .ensure_panes_exist(&source_runtime_session, &source_pane_ids)?;

        let source_and_target_are_grouped = (request.after || request.before)
            && matches!(
                (
                    self.sessions.session_group_name(&source_session_name),
                    self.sessions.session_group_name(&target_session_name),
                ),
                (Some(source_group), Some(target_group))
                    if source_session_name != target_session_name
                        && source_group == target_group
            );
        if source_and_target_are_grouped {
            return Err(RmuxError::Server("sessions are grouped".to_owned()));
        }

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
        let replaced_runtime_transfer_slot = match replaced_target.as_ref() {
            Some((_, link_count, _)) if *link_count > 1 => Some(
                self.linked_runtime_transfer_slot_for_detached_window(
                    &target_session_name,
                    target_window_index,
                )
                .ok_or_else(|| {
                    RmuxError::Server(format!(
                        "linked window {target_session_name}:{target_window_index} has no surviving slot"
                    ))
                })?,
            ),
            _ => None,
        };
        let replaced_pane_ids = replaced_target
            .as_ref()
            .map(|(pane_ids, _, _)| pane_ids.clone())
            .unwrap_or_default();

        let insertion_snapshot = SessionTransferSnapshot::capture(self);

        let session_mutation = (|| -> Result<_, RmuxError> {
            let session = self
                .sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?;
            let index_map = if request.after || request.before {
                session.make_room_for_window(target_window_index)?
            } else {
                std::collections::BTreeMap::new()
            };
            let inserted = session.link_window(
                target_window_index,
                source_window,
                request.kill_destination,
                !request.detached,
            )?;
            Ok((inserted, index_map))
        })();
        let (inserted, index_map) = match session_mutation {
            Ok(result) => result,
            Err(error) => {
                insertion_snapshot.restore(self);
                return Err(error);
            }
        };
        debug_assert!(existing_target_index_map.is_none() || index_map.is_empty());
        let group_index_map = existing_target_index_map.unwrap_or(&index_map);

        if let Err(error) =
            self.remap_session_group_window_metadata(&target_session_name, &index_map)
        {
            insertion_snapshot.restore(self);
            return Err(error);
        }
        let adjusted_source_window_index = if source_session_name == target_session_name {
            index_map
                .get(&request.source.window_index())
                .copied()
                .unwrap_or(request.source.window_index())
        } else {
            request.source.window_index()
        };

        let transaction = (|| -> Result<(), RmuxError> {
            if inserted.is_some() {
                if let Some((_, link_count, _)) = replaced_target.as_ref() {
                    let target =
                        WindowTarget::with_window(target_session_name.clone(), target_window_index);
                    let _ = self.options.remove_window(&target);
                    let _ = self.hooks.remove_window(&target);
                    self.clear_auto_named_window(&target_session_name, target_window_index);
                    if *link_count > 1 {
                        let _ =
                            self.detach_window_link_slot(&target_session_name, target_window_index);
                    }
                }
            }

            // Relative insertion can address a sparse slot that does not yet
            // exist in the target's runtime-owning group peer. Propagate the
            // inserted model before linked-window synchronization resolves
            // the canonical slot in that peer.
            self.synchronize_session_group_models_from_with_window_selection_map(
                &target_session_name,
                group_index_map,
            )?;

            self.attach_window_link_slot(
                &source_session_name,
                adjusted_source_window_index,
                &target_session_name,
                target_window_index,
            );

            #[cfg(test)]
            if std::mem::take(&mut self.fail_link_window_after_attach) {
                return Err(RmuxError::Server(
                    "injected link-window post-attach failure".to_owned(),
                ));
            }

            self.synchronize_linked_window_from_slot(
                &source_session_name,
                adjusted_source_window_index,
            )?;
            // The destination model must contain the linked WindowId before
            // option aliases are expanded by identity.
            self.synchronize_linked_window_options_from_slot(
                &source_session_name,
                adjusted_source_window_index,
            );
            for pane_index in &source_pane_indices {
                self.synchronize_pane_alias_options_from_target(&PaneTarget::with_window(
                    source_session_name.clone(),
                    adjusted_source_window_index,
                    *pane_index,
                ))?;
            }
            self.synchronize_session_group_from(&target_session_name)?;
            if source_session_name != target_session_name {
                self.synchronize_session_group_from(&source_session_name)?;
            }
            if let Some(source_session) = self.sessions.session(&source_session_name).cloned() {
                self.synchronize_pane_alias_options_from_session(&source_session)?;
            }

            // Runtime transfer/removal is the last fallible step. Transfer
            // rolls itself back if output-state migration fails, so restoring
            // the model snapshot remains sufficient on error.
            if inserted.is_some() {
                if let Some((pane_ids, link_count, runtime_session_name)) = replaced_target.as_ref()
                {
                    if let Some(survivor_slot) = replaced_runtime_transfer_slot.as_ref() {
                        let _ = self.transfer_detached_window_link_runtime(
                            runtime_session_name,
                            survivor_slot,
                            pane_ids,
                        )?;
                    } else if *link_count == 1 {
                        let _removed_terminals = self
                            .terminals
                            .remove_pane_batch(runtime_session_name, pane_ids)?;
                        for pane_id in pane_ids {
                            if let Some(pipe) =
                                self.remove_pane_pipe(runtime_session_name, *pane_id)
                            {
                                pipe.stop();
                            }
                        }
                        let mut removed_outputs =
                            self.remove_pane_outputs(runtime_session_name, pane_ids);
                        removed_outputs.abort_output_readers();
                        self.remove_pane_lifecycles(pane_ids);
                    }
                }
            }

            self.sync_pane_lifecycle_dimensions_for_session(&target_session_name);
            if source_session_name != target_session_name {
                self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
            }
            Ok(())
        })();
        if let Err(error) = transaction {
            insertion_snapshot.restore(self);
            return Err(error);
        }

        Ok(LinkedWindowResult {
            response: LinkWindowResponse {
                target: WindowTarget::with_window(target_session_name, target_window_index),
            },
            removed_pane_ids: self.pane_ids_no_longer_referenced(replaced_pane_ids),
            reindexed_windows: index_map,
        })
    }

    pub(crate) fn unlink_window(
        &mut self,
        target: WindowTarget,
        kill_if_last: bool,
    ) -> Result<UnlinkedWindowResult, RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let grouped_timer_targets = self
            .sessions
            .session_group_members(&session_name)
            .into_iter()
            .map(|session_name| WindowTarget::with_window(session_name, window_index))
            .collect::<Vec<_>>();
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
            let removed_timer_targets = killed
                .removed_windows
                .iter()
                .map(|removed| removed.target.clone())
                .collect();
            return Ok(UnlinkedWindowResult {
                response: UnlinkWindowResponse {
                    target: killed.response.target,
                },
                removed_window,
                removed_pane_ids: killed.removed_pane_ids,
                removed_timer_targets,
                reindexed_windows: killed.reindexed_windows,
            });
        }

        let runtime_transfer_slot = self
            .linked_runtime_transfer_slot_for_detached_window(&session_name, window_index)
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "linked window {session_name}:{window_index} has no surviving slot"
                ))
            })?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        let pane_ids = self
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(window_index))
            .map(|window| {
                window
                    .panes()
                    .iter()
                    .map(|pane| pane.id())
                    .collect::<Vec<_>>()
            })
            .expect("unlink target was already validated");
        let snapshot = SessionTransferSnapshot::capture(self);

        let transaction = (|| -> Result<u32, RmuxError> {
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
            let _ = self.transfer_detached_window_link_runtime(
                &runtime_session_name,
                &runtime_transfer_slot,
                &pane_ids,
            )?;
            Ok(active_window)
        })();
        let active_window = match transaction {
            Ok(active_window) => active_window,
            Err(error) => {
                snapshot.restore(self);
                return Err(error);
            }
        };

        Ok(UnlinkedWindowResult {
            response: UnlinkWindowResponse {
                target: WindowTarget::with_window(session_name, active_window),
            },
            removed_window,
            removed_pane_ids: Vec::new(),
            removed_timer_targets: grouped_timer_targets,
            reindexed_windows: Vec::new(),
        })
    }
}
