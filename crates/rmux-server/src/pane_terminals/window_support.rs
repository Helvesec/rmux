use rmux_proto::{
    KillWindowResponse, LastWindowResponse, NewWindowResponse, NextWindowResponse, OptionName,
    PaneId, PreviousWindowResponse, RenameWindowResponse, RmuxError, ScopeSelector,
    SelectWindowResponse, SessionName, SetOptionMode, WindowTarget,
};
use std::collections::HashSet;

mod window_link_commands;
mod window_movement;
mod window_respawn;

use super::{
    session_not_found, HandlerState, KilledWindowResult, NewWindowOptions,
    RemovedWindowHookContext, SessionTransferSnapshot,
};

mod window_removal;

use window_removal::build_window_removal_plan;
pub(super) use window_removal::window_pane_ids;

pub(crate) struct RespawnWindowResult {
    pub(crate) response: rmux_proto::RespawnWindowResponse,
    pub(crate) retained_pane_id: PaneId,
    pub(crate) removed_pane_ids: Vec<PaneId>,
    pub(crate) refresh_sessions: Vec<SessionName>,
}

impl HandlerState {
    pub(crate) fn create_window(
        &mut self,
        session_name: &SessionName,
        options: NewWindowOptions<'_>,
    ) -> Result<NewWindowResponse, RmuxError> {
        self.create_window_at_requested_index(session_name, None, false, options)
    }

    pub(crate) fn create_window_at_requested_index(
        &mut self,
        session_name: &SessionName,
        target_window_index: Option<u32>,
        insert_at_target: bool,
        options: NewWindowOptions<'_>,
    ) -> Result<NewWindowResponse, RmuxError> {
        let NewWindowOptions {
            name,
            detached,
            spawn,
        } = options;
        let explicit_name = name.is_some();
        let previous_session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        ensure_session_panes_exist(self, session_name, previous_session)?;
        let size = previous_session.window().size();

        let base_index = self
            .options
            .resolve(Some(session_name), OptionName::BaseIndex)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let mutation_snapshot =
            SessionTransferSnapshot::capture(self, std::slice::from_ref(session_name))?;
        let session_mutation = (|| -> Result<_, RmuxError> {
            let index_map = if let Some(index) = target_window_index.filter(|_| insert_at_target) {
                self.sessions
                    .make_room_for_window(session_name, index)?
                    .sessions
                    .into_iter()
                    .find(|change| {
                        self.sessions
                            .session(session_name)
                            .is_some_and(|session| session.id() == change.session_id)
                    })
                    .expect("addressed session change")
                    .index_map
            } else {
                std::collections::BTreeMap::new()
            };
            let created = match target_window_index {
                Some(index) => self
                    .sessions
                    .insert_window_with_initial_pane(session_name, index, size)
                    .map_err(|error| match error {
                        RmuxError::InvalidTarget { ref reason, .. }
                            if reason == "window index already exists in session" =>
                        {
                            RmuxError::Server(format!("create window failed: index {index} in use"))
                        }
                        error => error,
                    })?,
                None => self
                    .sessions
                    .create_window_at_or_above(session_name, size, base_index)?,
            };
            let window_index = created.window_index;
            let pane_id = created.pane_id;
            let target = WindowTarget::with_window(session_name.clone(), window_index);
            if let Some(name) = name {
                let _ = self.sessions.rename_window(&target, name)?;
            }
            if !detached {
                self.sessions.select_window(&target)?;
            }
            Ok((window_index, pane_id, index_map))
        })();
        let (window_index, pane_id, index_map) = match session_mutation {
            Ok(result) => result,
            Err(error) => {
                mutation_snapshot.restore(self)?;
                return Err(error);
            }
        };

        if let Err(error) = self.remap_session_group_window_metadata(session_name, &index_map) {
            mutation_snapshot.restore(self)?;
            return Err(error);
        }

        if let Err(error) = self.insert_window_terminal(session_name, window_index, spawn) {
            mutation_snapshot.restore(self)?;
            return Err(error);
        }
        let target = WindowTarget::with_window(session_name.clone(), window_index);
        if explicit_name {
            self.disable_automatic_rename_for_window(&target)?;
        }

        debug_assert_eq!(
            self.sessions
                .session(session_name)
                .and_then(|session| session.pane_id_in_window(window_index, 0)),
            Some(pane_id)
        );
        self.synchronize_session_group_from_with_window_selection_map(session_name, &index_map)?;
        self.sync_pane_lifecycle_dimensions_for_session(session_name);

        Ok(NewWindowResponse { target })
    }

    pub(crate) fn kill_window(
        &mut self,
        target: WindowTarget,
        kill_others: bool,
    ) -> Result<KilledWindowResult, RmuxError> {
        let session_name = target.session_name().clone();
        let target_index = target.window_index();
        let (removal_plan, removed_windows) = {
            let session = self
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            let removal_plan =
                build_window_removal_plan(self, session, &session_name, target_index, kill_others)?;
            let removed_windows = removal_plan
                .iter()
                .map(|planned_window| {
                    let window = self
                        .sessions
                        .session(&planned_window.session_name)
                        .and_then(|session| session.window_at(planned_window.window_index))
                        .ok_or_else(|| {
                            RmuxError::invalid_target(
                                format!(
                                    "{}:{}",
                                    planned_window.session_name, planned_window.window_index
                                ),
                                "window index does not exist in session",
                            )
                        })?;
                    Ok(RemovedWindowHookContext {
                        target: WindowTarget::with_window(
                            planned_window.session_name.clone(),
                            planned_window.window_index,
                        ),
                        window_id: window.id().as_u32(),
                        window_name: window.name().unwrap_or_default().to_owned(),
                    })
                })
                .collect::<Result<Vec<_>, RmuxError>>()?;
            (removal_plan, removed_windows)
        };
        let removed_pane_ids = removal_plan
            .iter()
            .flat_map(|planned_window| planned_window.pane_ids.iter().copied())
            .collect::<Vec<_>>();

        let mut removals_by_session = std::collections::HashMap::<SessionName, usize>::new();
        for planned_window in &removal_plan {
            *removals_by_session
                .entry(planned_window.session_name.clone())
                .or_default() += 1;
        }
        let mut destroyed_session_names = removal_plan
            .iter()
            .filter_map(|planned_window| {
                let session_name = &planned_window.session_name;
                let removed_count = removals_by_session.get(session_name).copied()?;
                self.sessions
                    .session(session_name)
                    .is_some_and(|session| session.windows().count() == removed_count)
                    .then(|| session_name.clone())
            })
            .collect::<Vec<_>>();
        let mut seen_destroyed_sessions = HashSet::new();
        destroyed_session_names
            .retain(|session_name| seen_destroyed_sessions.insert(session_name.clone()));
        let removed_window_ids = removed_windows
            .iter()
            .map(|removed_window| rmux_proto::WindowId::new(removed_window.window_id))
            .collect::<Vec<_>>();

        let sessions_to_synchronize = removal_plan
            .iter()
            .map(|planned_window| planned_window.session_name.clone())
            .collect::<HashSet<_>>();
        let mut removed_terminals = HashSet::new();
        for planned_window in removal_plan {
            let planned_target = WindowTarget::with_window(
                planned_window.session_name.clone(),
                planned_window.window_index,
            );
            let _removed_window = self
                .sessions
                .remove_window_occurrence_for_teardown(&planned_target)?;
            let _ = self.options.remove_window(&planned_target);
            let _ = self.hooks.remove_window(&planned_target);
            self.clear_auto_named_window(&planned_window.session_name, planned_window.window_index);
            let _ = self
                .detach_window_link_slot(&planned_window.session_name, planned_window.window_index);

            for pane_id in planned_window.pane_ids {
                if !removed_terminals.insert((planned_window.runtime_session_name.clone(), pane_id))
                {
                    continue;
                }
                if !self.remove_pane_terminal_from_runtime(
                    &planned_window.runtime_session_name,
                    pane_id,
                ) {
                    return Err(RmuxError::Server(format!(
                        "missing pane terminal for pane id {} in session {}",
                        pane_id.as_u32(),
                        planned_window.runtime_session_name
                    )));
                }
            }
        }

        let mut destroyed_sessions = Vec::new();
        let mut session_removal_order = destroyed_session_names.clone();
        session_removal_order.sort_by(|left, right| {
            let left_is_owner = self.sessions.runtime_owner(left).as_ref() == Some(left);
            let right_is_owner = self.sessions.runtime_owner(right).as_ref() == Some(right);
            left_is_owner
                .cmp(&right_is_owner)
                .then_with(|| left.as_str().cmp(right.as_str()))
        });
        for destroyed_session_name in session_removal_order {
            let current_runtime_owner = self.sessions.runtime_owner(&destroyed_session_name);
            let next_runtime_owner = self
                .sessions
                .runtime_owner_transfer_target(&destroyed_session_name);
            let removed_session = self.sessions.remove_session(&destroyed_session_name)?;
            destroyed_sessions.push((destroyed_session_name.clone(), removed_session.id()));
            let _ = self.options.remove_session(&destroyed_session_name);
            let _ = self.environment.remove_session(&destroyed_session_name);
            self.remove_session_terminals(
                &destroyed_session_name,
                current_runtime_owner.as_ref(),
                next_runtime_owner.as_ref(),
            )?;
        }
        destroyed_sessions.sort_by_key(|(destroyed_session_name, _)| {
            destroyed_session_names
                .iter()
                .position(|candidate| candidate == destroyed_session_name)
                .unwrap_or(usize::MAX)
        });

        let mut reindexed_windows = Vec::new();
        for synchronized_session in &sessions_to_synchronize {
            if destroyed_session_names.contains(synchronized_session) {
                continue;
            }
            let index_map = self.renumber_windows_if_enabled(synchronized_session)?;
            if !index_map.is_empty() {
                reindexed_windows.push((synchronized_session.clone(), index_map));
            }
        }
        let active_window = self
            .sessions
            .session(&session_name)
            .map_or(target_index, |session| session.active_window_index());
        for synchronized_session in sessions_to_synchronize {
            if destroyed_session_names.contains(&synchronized_session) {
                continue;
            }
            self.synchronize_session_group_from(&synchronized_session)?;
        }
        let removed_pane_ids = self.pane_ids_no_longer_referenced(removed_pane_ids);

        Ok(KilledWindowResult {
            response: KillWindowResponse {
                target: WindowTarget::with_window(session_name, active_window),
            },
            removed_windows,
            removed_pane_ids,
            destroyed_sessions,
            removed_window_ids,
            reindexed_windows,
        })
    }

    pub(crate) fn select_window(
        &mut self,
        target: WindowTarget,
    ) -> Result<SelectWindowResponse, RmuxError> {
        self.sessions.select_window(&target)?;

        Ok(SelectWindowResponse { target })
    }

    pub(crate) fn rename_window(
        &mut self,
        target: WindowTarget,
        new_name: String,
    ) -> Result<RenameWindowResponse, RmuxError> {
        let _ = self.sessions.rename_window(&target, new_name)?;
        self.disable_automatic_rename_for_window(&target)?;
        self.synchronize_linked_window_options_from_slot(
            target.session_name(),
            target.window_index(),
        );
        self.clear_auto_named_window_family(target.session_name(), target.window_index());
        self.synchronize_window_alias_family_from_slot(
            target.session_name(),
            target.window_index(),
        )?;

        Ok(RenameWindowResponse { target })
    }

    pub(crate) fn disable_automatic_rename_for_window(
        &mut self,
        target: &WindowTarget,
    ) -> Result<(), RmuxError> {
        self.options.set(
            ScopeSelector::Window(target.clone()),
            OptionName::AutomaticRename,
            "off".to_owned(),
            SetOptionMode::Replace,
        )?;
        Ok(())
    }

    pub(crate) fn next_window(
        &mut self,
        session_name: &SessionName,
        alerts_only: bool,
    ) -> Result<NextWindowResponse, RmuxError> {
        let window_index = self
            .sessions
            .select_adjacent_window(session_name, true, alerts_only)?;

        Ok(NextWindowResponse {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        })
    }

    pub(crate) fn previous_window(
        &mut self,
        session_name: &SessionName,
        alerts_only: bool,
    ) -> Result<PreviousWindowResponse, RmuxError> {
        let window_index =
            self.sessions
                .select_adjacent_window(session_name, false, alerts_only)?;

        Ok(PreviousWindowResponse {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        })
    }

    pub(crate) fn last_window(
        &mut self,
        session_name: &SessionName,
    ) -> Result<LastWindowResponse, RmuxError> {
        let window_index = self.sessions.last_window(session_name)?;

        Ok(LastWindowResponse {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        })
    }

    pub(crate) fn resize_window(
        &mut self,
        request: rmux_proto::ResizeWindowRequest,
        maximum: Option<rmux_proto::TerminalSize>,
    ) -> Result<rmux_proto::ResizeWindowResponse, RmuxError> {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let target = request.target;
        let response_target = target.clone();
        let (response, _) = self.mutate_window_layout_and_resize_terminal_with_family(
            &target,
            |mut window_layout| {
                window_layout.resize_window(request.width, request.height, request.adjustment);
                if let Some(maximum) = maximum {
                    window_layout.constrain_size(maximum);
                }
                Ok(rmux_proto::ResizeWindowResponse {
                    target: response_target,
                })
            },
        )?;
        self.options.set(
            ScopeSelector::Window(target),
            OptionName::WindowSize,
            "manual".to_owned(),
            SetOptionMode::Replace,
        )?;
        self.synchronize_linked_window_options_from_slot(&session_name, window_index);
        Ok(response)
    }
}

fn link_window_destination_index(
    session: rmux_core::ManagedSessionView<'_>,
    target_window_index: u32,
    after: bool,
    before: bool,
) -> Result<u32, RmuxError> {
    if !(after || before) {
        return Ok(target_window_index);
    }

    if session.window_at(target_window_index).is_none() {
        return Err(RmuxError::invalid_target(
            format!("{}:{target_window_index}", session.name()),
            "window index does not exist in session",
        ));
    }

    if before {
        Ok(target_window_index)
    } else {
        target_window_index.checked_add(1).ok_or_else(|| {
            RmuxError::Server(format!(
                "window index space exhausted for session {}",
                session.name()
            ))
        })
    }
}

fn request_target_string(target: &rmux_proto::MoveWindowTarget) -> String {
    match target {
        rmux_proto::MoveWindowTarget::Session(session_name) => session_name.to_string(),
        rmux_proto::MoveWindowTarget::Window(target) => target.to_string(),
    }
}

fn ensure_session_panes_exist(
    state: &HandlerState,
    session_name: &SessionName,
    session: rmux_core::ManagedSessionView<'_>,
) -> Result<(), RmuxError> {
    for (window_index, window) in session.windows() {
        let pane_ids = window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>();
        if !pane_ids.is_empty() {
            state.ensure_window_panes_exist(session_name, window_index, &pane_ids)?;
        }
    }
    Ok(())
}
