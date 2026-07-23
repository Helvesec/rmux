use rmux_core::{OptionStore, Session};
use rmux_proto::{
    KillWindowResponse, LastWindowResponse, NewWindowResponse, NextWindowResponse, OptionName,
    PaneId, PreviousWindowResponse, RenameWindowResponse, RmuxError, ScopeSelector,
    SelectWindowResponse, SessionName, SetOptionMode, WindowTarget,
};
use std::collections::HashSet;

#[path = "pane_terminals/window_link_commands.rs"]
mod window_link_commands;
#[path = "pane_terminals/window_movement.rs"]
mod window_movement;

use super::{
    session_not_found, HandlerState, KilledWindowResult, NewWindowOptions, PreparedWindowTerminal,
    RemovedWindowHookContext, RespawnWindowOptions, SessionTransferSnapshot, WindowSpawnOptions,
};
use crate::terminal::validate_process_command;

#[path = "pane_terminals/window_removal.rs"]
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
            .cloned()
            .ok_or_else(|| session_not_found(session_name))?;
        ensure_session_panes_exist(self, session_name, &previous_session)?;
        let size = previous_session.window().size();

        let base_index = self
            .options
            .resolve(Some(session_name), OptionName::BaseIndex)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let mutation_snapshot = SessionTransferSnapshot::capture(self);
        let pane_id = self.sessions.allocate_pane_id();
        let session_mutation = (|| -> Result<_, RmuxError> {
            let session = self
                .sessions
                .session_mut(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let (window_index, pane_id, index_map) = match target_window_index {
                Some(window_index) => {
                    let index_map = if insert_at_target {
                        session.make_room_for_window(window_index)?
                    } else if session.window_at(window_index).is_some() {
                        return Err(RmuxError::Server(format!(
                            "create window failed: index {window_index} in use"
                        )));
                    } else {
                        std::collections::BTreeMap::new()
                    };
                    session.insert_window_with_initial_pane_with_id(window_index, size, pane_id)?;
                    (window_index, pane_id, index_map)
                }
                None => {
                    let (window_index, pane_id) = session
                        .create_window_at_or_above_with_pane_id(size, base_index, pane_id)?;
                    (window_index, pane_id, std::collections::BTreeMap::new())
                }
            };
            if let Some(name) = name {
                session.rename_window(window_index, name)?;
            }
            if !detached {
                session.select_window(window_index)?;
            }
            Ok((window_index, pane_id, index_map))
        })();
        let (window_index, pane_id, index_map) = match session_mutation {
            Ok(result) => result,
            Err(error) => {
                mutation_snapshot.restore(self);
                return Err(error);
            }
        };

        if let Err(error) = self.remap_session_group_window_metadata(session_name, &index_map) {
            mutation_snapshot.restore(self);
            return Err(error);
        }

        if let Err(error) = self.insert_window_terminal(session_name, window_index, spawn) {
            mutation_snapshot.restore(self);
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
                    .is_some_and(|session| session.windows().len() == removed_count)
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
                .session_mut(&planned_window.session_name)
                .ok_or_else(|| session_not_found(&planned_window.session_name))?
                .remove_window_allowing_empty(planned_window.window_index)?;
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
            .map_or(target_index, Session::active_window_index);
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
        let session = self
            .sessions
            .session_mut(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))?;
        // Session::select_window already clears alert flags on the newly-selected window.
        session.select_window(target.window_index())?;

        Ok(SelectWindowResponse { target })
    }

    pub(crate) fn rename_window(
        &mut self,
        target: WindowTarget,
        new_name: String,
    ) -> Result<RenameWindowResponse, RmuxError> {
        {
            let session = self
                .sessions
                .session_mut(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?;
            session.rename_window(target.window_index(), new_name)?;
        }
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
        let session = self
            .sessions
            .session_mut(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let window_index = if alerts_only {
            session.next_window_with_alerts()?
        } else {
            session.next_window()?
        };

        Ok(NextWindowResponse {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        })
    }

    pub(crate) fn previous_window(
        &mut self,
        session_name: &SessionName,
        alerts_only: bool,
    ) -> Result<PreviousWindowResponse, RmuxError> {
        let session = self
            .sessions
            .session_mut(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let window_index = if alerts_only {
            session.previous_window_with_alerts()?
        } else {
            session.previous_window()?
        };

        Ok(PreviousWindowResponse {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        })
    }

    pub(crate) fn last_window(
        &mut self,
        session_name: &SessionName,
    ) -> Result<LastWindowResponse, RmuxError> {
        let session = self
            .sessions
            .session_mut(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let window_index = session.last_window()?;

        Ok(LastWindowResponse {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        })
    }

    pub(crate) fn resize_window(
        &mut self,
        request: rmux_proto::ResizeWindowRequest,
    ) -> Result<rmux_proto::ResizeWindowResponse, RmuxError> {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();

        self.mutate_session_and_resize_window_terminal(&session_name, window_index, |session| {
            let current_size = session
                .window_at(window_index)
                .ok_or_else(|| {
                    RmuxError::invalid_target(
                        format!("{session_name}:{window_index}"),
                        "window index does not exist in session",
                    )
                })?
                .size();

            let mut sx = current_size.cols;
            let mut sy = current_size.rows;

            if let Some(width) = request.width {
                sx = width;
            }
            if let Some(height) = request.height {
                sy = height;
            }

            if let Some(adjustment) = request.adjustment {
                use rmux_proto::ResizeWindowAdjustment;
                match adjustment {
                    ResizeWindowAdjustment::Left(amount) => {
                        sx = sx.saturating_sub(amount);
                    }
                    ResizeWindowAdjustment::Right(amount) => {
                        sx = sx.saturating_add(amount);
                    }
                    ResizeWindowAdjustment::Up(amount) => {
                        sy = sy.saturating_sub(amount);
                    }
                    ResizeWindowAdjustment::Down(amount) => {
                        sy = sy.saturating_add(amount);
                    }
                    ResizeWindowAdjustment::LargestLinkedSession
                    | ResizeWindowAdjustment::SmallestLinkedSession => {}
                }
            }

            sx = sx.max(1);
            sy = sy.max(1);

            session.resize_window(
                window_index,
                rmux_proto::TerminalSize { cols: sx, rows: sy },
            )?;

            Ok(rmux_proto::ResizeWindowResponse {
                target: request.target.clone(),
            })
        })
    }

    pub(crate) fn respawn_window(
        &mut self,
        target: rmux_proto::WindowTarget,
        options: RespawnWindowOptions<'_>,
    ) -> Result<RespawnWindowResult, RmuxError> {
        let RespawnWindowOptions { kill, spawn } = options;
        let session_name = target.session_name().clone();
        let window_index = target.window_index();

        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        let pane_ids = window_pane_ids(&previous_session, &session_name, window_index)?;

        // Without -k, reject if any pane terminal is still present (i.e. process may be running).
        if !kill
            && pane_ids.iter().any(|id| {
                self.ensure_window_panes_exist(&session_name, window_index, &[*id])
                    .is_ok()
            })
        {
            return Err(RmuxError::Server(
                "window still active; use -k to force respawn".to_owned(),
            ));
        }

        let pane_id = pane_ids
            .first()
            .copied()
            .ok_or_else(|| RmuxError::Server("window has no panes".to_owned()))?;
        let provenance = self.pane_respawn_provenance(pane_id);
        let process_command = spawn.command.cloned().or_else(|| {
            provenance
                .as_ref()
                .and_then(|provenance| provenance.process_command.clone())
        });
        validate_process_command(process_command.as_ref())?;
        let start_directory = spawn
            .start_directory
            .map(std::path::Path::to_path_buf)
            .or_else(|| {
                provenance
                    .as_ref()
                    .and_then(|provenance| provenance.working_directory.clone())
            });
        let respawn_environment = provenance
            .as_ref()
            .map(|provenance| provenance.private_environment.clone());
        let respawn_shell = provenance
            .as_ref()
            .map(|provenance| provenance.shell.as_path());
        let environment_overrides = spawn
            .environment_overrides
            .map(<[String]>::to_vec)
            .or_else(|| respawn_environment.clone());
        let spawn = WindowSpawnOptions {
            start_directory: start_directory.as_deref(),
            command: process_command.as_ref(),
            socket_path: spawn.socket_path,
            spawn_environment: spawn.spawn_environment,
            environment_overrides: environment_overrides.as_deref(),
            respawn_shell,
            respawn_environment: respawn_environment.as_deref(),
            pane_alert_callback: spawn.pane_alert_callback,
            pane_exit_callback: spawn.pane_exit_callback,
        };
        let removed_pane_ids = pane_ids
            .iter()
            .copied()
            .filter(|id| *id != pane_id)
            .collect::<Vec<_>>();
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        let previous_output_generation =
            self.pane_output_generation(&runtime_session_name, pane_id);
        let base_environment =
            self.session_base_environment_for_window(&session_name, window_index);
        let mut pane_option_sessions =
            self.window_linked_session_family_list(&session_name, window_index);
        if pane_option_sessions.is_empty() {
            pane_option_sessions.push(session_name.clone());
        }
        pane_option_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        pane_option_sessions.dedup();
        let before_pane_options = pane_option_sessions
            .into_iter()
            .map(|affected_session| {
                self.pane_option_slots_for_session(&affected_session)
                    .map(|snapshot| (affected_session, snapshot))
            })
            .collect::<Result<Vec<_>, RmuxError>>()?;

        // Prepare the replacement process against a preview of the new layout.
        // No old runtime state is touched until every fallible spawn step has
        // succeeded, so profile/PTY/output-reader failures are true no-ops.
        let mut respawned_session = previous_session.clone();
        respawned_session.respawn_window_with_pane_id(window_index, pane_id)?;
        respawned_session.select_window(window_index)?;
        let prepared = self.prepare_window_terminal(
            &respawned_session,
            window_index,
            spawn,
            base_environment.as_ref(),
        )?;
        let automatic_name_applied = apply_prepared_automatic_window_name(
            &self.options,
            self.tracks_auto_named_window(&session_name, window_index),
            &mut respawned_session,
            window_index,
            &prepared,
        );

        let mut removed_terminals = pane_ids
            .iter()
            .filter_map(|pane_id| {
                self.terminals
                    .remove_pane(&runtime_session_name, *pane_id)
                    .map(|terminal| (*pane_id, terminal))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut removed_outputs = self.remove_pane_outputs(&runtime_session_name, &pane_ids);
        // Keep the output channel for the stable pane identity. Existing SDK
        // subscribers own receivers for this sender; replacing the channel
        // would leave a registry record that can never observe the respawned
        // process. Generation advancement below rejects any late output from
        // the old reader while preserving the receiver identity.
        let retained_output_sender = removed_outputs.pane_output_sender(pane_id);
        self.seed_pane_output_generation(
            &runtime_session_name,
            pane_id,
            previous_output_generation,
        );
        self.replace_session(&session_name, respawned_session)?;

        if let Err(error) = self.install_prepared_window_terminal(
            &runtime_session_name,
            window_index,
            prepared,
            retained_output_sender,
        ) {
            self.replace_session(&session_name, previous_session)?;
            self.terminals
                .insert_existing_panes(&runtime_session_name, removed_terminals)?;
            self.insert_existing_pane_outputs(&runtime_session_name, removed_outputs);
            return Err(error);
        }

        for old_pane_id in &pane_ids {
            if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, *old_pane_id) {
                pipe.stop();
            }
        }
        for removed_pane_id in &removed_pane_ids {
            self.clear_marked_pane_if_id(*removed_pane_id);
        }
        self.remove_pane_lifecycles(&removed_pane_ids);
        if automatic_name_applied {
            self.mark_auto_named_window(&session_name, window_index);
        }
        removed_outputs.abort_output_readers();
        super::terminate_removed_terminals(&mut removed_terminals);

        // The window model is shared through explicit link aliases as well as
        // session groups. Synchronize every alias before deciding which pane
        // identities disappeared; otherwise a linked slot can keep destroyed
        // sibling panes artificially reachable after the runtime was replaced.
        let mut synchronized_sessions =
            self.synchronize_linked_window_family_from_slot(&session_name, window_index)?;
        synchronized_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        synchronized_sessions.dedup();
        for synchronized_session in &synchronized_sessions {
            self.sync_pane_lifecycle_dimensions_for_session(synchronized_session);
        }
        for (affected_session, before) in before_pane_options {
            self.rekey_pane_options_after_session_change(&before, &affected_session)?;
        }
        let removed_pane_ids = self.pane_ids_no_longer_referenced(removed_pane_ids);

        Ok(RespawnWindowResult {
            response: rmux_proto::RespawnWindowResponse { target },
            retained_pane_id: pane_id,
            removed_pane_ids,
            refresh_sessions: synchronized_sessions,
        })
    }
}

fn apply_prepared_automatic_window_name(
    options: &OptionStore,
    tracked: bool,
    session: &mut Session,
    window_index: u32,
    prepared: &PreparedWindowTerminal,
) -> bool {
    let Some(name) = prepared.automatic_window_name() else {
        return false;
    };
    let session_name = session.name().clone();
    let should_apply = session.window_at(window_index).is_some_and(|window| {
        window.name().is_none()
            && crate::automatic_rename::window_allows_automatic_rename(
                options,
                &session_name,
                window_index,
                window,
                tracked,
            )
    });
    if should_apply {
        session
            .window_at_mut(window_index)
            .expect("prevalidated respawn window exists")
            .set_automatic_name(name.to_owned());
    }
    should_apply
}

fn link_window_destination_index(
    session: &Session,
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
    session: &Session,
) -> Result<(), RmuxError> {
    for (window_index, window) in session.windows() {
        let pane_ids = window
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect::<Vec<_>>();
        if !pane_ids.is_empty() {
            state.ensure_window_panes_exist(session_name, *window_index, &pane_ids)?;
        }
    }
    Ok(())
}
