use std::collections::{HashMap, HashSet};

use rmux_core::{
    formats::{render_list_windows_line, FormatContext},
    PaneId, Session,
};
use rmux_proto::{
    CommandOutput, KillWindowResponse, LastWindowResponse, ListWindowsResponse, NewWindowResponse,
    NextWindowResponse, OptionName, PreviousWindowResponse, RenameWindowResponse, RmuxError,
    SelectWindowResponse, SessionName, WindowListEntry, WindowTarget,
};

#[path = "pane_terminals/window_link_commands.rs"]
mod window_link_commands;
#[path = "pane_terminals/window_movement.rs"]
mod window_movement;

use super::{
    session_not_found, HandlerState, KilledWindowResult, NewWindowOptions,
    RemovedWindowHookContext, RespawnWindowOptions, WindowLinkSlot,
};
use crate::format_runtime::RuntimeFormatContext;

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
        let pane_id = self.sessions.allocate_pane_id();
        let (window_index, pane_id) = {
            let session = self
                .sessions
                .session_mut(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let (window_index, pane_id) = match target_window_index {
                Some(window_index) => {
                    if insert_at_target {
                        session.make_room_for_window(window_index)?;
                    } else if session.window_at(window_index).is_some() {
                        return Err(RmuxError::Server(format!(
                            "create window failed: index {window_index} in use"
                        )));
                    }
                    session.insert_window_with_initial_pane_with_id(window_index, size, pane_id)?;
                    (window_index, pane_id)
                }
                None => {
                    session.create_window_at_or_above_with_pane_id(size, base_index, pane_id)?
                }
            };
            if let Some(name) = name {
                session.rename_window(window_index, name)?;
            }
            if !detached {
                session.select_window(window_index)?;
            }
            (window_index, pane_id)
        };

        if let Err(error) = self.insert_window_terminal(session_name, window_index, spawn) {
            self.replace_session(session_name, previous_session)?;
            return Err(error);
        }

        debug_assert_eq!(
            self.sessions
                .session(session_name)
                .and_then(|session| session.pane_id_in_window(window_index, 0)),
            Some(pane_id)
        );
        self.synchronize_session_group_from(session_name)?;
        self.sync_pane_lifecycle_dimensions_for_session(session_name);

        Ok(NewWindowResponse {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        })
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
        ensure_window_removal_terminals_exist(self, &removal_plan)?;
        let removed_pane_ids = removal_plan
            .iter()
            .flat_map(|planned_window| planned_window.pane_ids.iter().copied())
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
                .remove_window(planned_window.window_index)?;
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

        let active_window = self
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?
            .active_window_index();
        for synchronized_session in sessions_to_synchronize {
            self.synchronize_session_group_from(&synchronized_session)?;
        }

        Ok(KilledWindowResult {
            response: KillWindowResponse {
                target: WindowTarget::with_window(session_name, active_window),
            },
            removed_windows,
            removed_pane_ids,
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
        self.clear_auto_named_window_family(target.session_name(), target.window_index());
        self.synchronize_linked_window_from_slot(target.session_name(), target.window_index())?;
        self.synchronize_session_group_from(target.session_name())?;

        Ok(RenameWindowResponse { target })
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

        self.mutate_session_and_resize_terminals(&session_name, |session| {
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
    ) -> Result<rmux_proto::RespawnWindowResponse, RmuxError> {
        let RespawnWindowOptions { kill, spawn } = options;
        let session_name = target.session_name().clone();
        let window_index = target.window_index();

        // Check that the window exists and collect its pane IDs.
        let pane_ids = {
            let session = self
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            let window = session.window_at(window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{session_name}:{window_index}"),
                    "window index does not exist in session",
                )
            })?;
            window.panes().iter().map(|p| p.id()).collect::<Vec<_>>()
        };

        // Without -k, reject if any pane terminal is still present (i.e. process may be running).
        if !kill
            && pane_ids
                .iter()
                .any(|id| self.ensure_panes_exist(&session_name, &[*id]).is_ok())
        {
            return Err(RmuxError::Server(
                "window still active; use -k to force respawn".to_owned(),
            ));
        }

        let pane_id = pane_ids
            .first()
            .copied()
            .ok_or_else(|| RmuxError::Server("window has no panes".to_owned()))?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);

        // Kill terminals for panes that disappear with the old window layout.
        for removed_pane_id in pane_ids.iter().copied().filter(|id| *id != pane_id) {
            self.remove_pane_terminal_from_runtime(&runtime_session_name, removed_pane_id);
        }
        if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, pane_id) {
            pipe.stop();
        }
        let _ = self.terminals.remove_pane(&runtime_session_name, pane_id);

        // tmux respawns a window by retaining the first pane's identity and
        // destroying the rest, rather than allocating a new pane identity.
        {
            let session = self
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            session.respawn_window_with_pane_id(window_index, pane_id)?;
            session.select_window(window_index)?;
        }

        // Spawn the new terminal for the single fresh pane.
        self.reset_window_terminal(&session_name, window_index, spawn)?;

        self.synchronize_session_group_from(&session_name)?;
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        Ok(rmux_proto::RespawnWindowResponse { target })
    }

    pub(crate) fn list_windows(
        &self,
        session_name: &SessionName,
        format: Option<&str>,
        attached_count: usize,
    ) -> Result<ListWindowsResponse, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let windows = collect_window_entries(self, session, session_name, format, attached_count);
        let output = build_command_output(&windows);

        Ok(ListWindowsResponse { windows, output })
    }
}

fn collect_window_entries(
    state: &HandlerState,
    session: &Session,
    session_name: &SessionName,
    format: Option<&str>,
    attached_count: usize,
) -> Vec<WindowListEntry> {
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let session_context =
        FormatContext::from_session(session).with_session_attached(attached_count);

    session
        .windows()
        .iter()
        .map(|(window_index, window)| {
            let active = *window_index == active_window;
            let last = Some(*window_index) == last_window;
            let mut context =
                session_context
                    .clone()
                    .with_window(*window_index, window, active, last);
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(*window_index, window);
            if let Some(pane) = window.active_pane() {
                runtime = runtime.with_pane(pane);
            }
            if attached_count == 0 {
                runtime = runtime.with_unclipped_geometry();
            }
            let rendered = render_list_windows_line(&runtime, format);

            WindowListEntry {
                target: WindowTarget::with_window(session_name.clone(), *window_index),
                window_id: window.id().to_string(),
                name: window.name().map(str::to_owned),
                pane_count: u32::try_from(window.pane_count()).expect("pane count fits in u32"),
                size: window.size(),
                layout: window.layout(),
                active,
                last,
                rendered,
            }
        })
        .collect()
}

fn build_command_output(windows: &[WindowListEntry]) -> CommandOutput {
    let stdout = windows
        .iter()
        .map(|window| window.rendered.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let stdout = if stdout.is_empty() {
        Vec::new()
    } else {
        format!("{stdout}\n").into_bytes()
    };

    CommandOutput::from_stdout(stdout)
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

fn window_pane_ids(
    session: &Session,
    session_name: &SessionName,
    window_index: u32,
) -> Result<Vec<PaneId>, RmuxError> {
    let window = session.window_at(window_index).ok_or_else(|| {
        RmuxError::invalid_target(
            format!("{session_name}:{window_index}"),
            "window index does not exist in session",
        )
    })?;

    Ok(window.panes().iter().map(|pane| pane.id()).collect())
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

#[derive(Debug, Clone)]
struct WindowRemovalPlan {
    session_name: SessionName,
    window_index: u32,
    runtime_session_name: SessionName,
    pane_ids: Vec<PaneId>,
}

fn build_window_removal_plan(
    state: &HandlerState,
    session: &Session,
    session_name: &SessionName,
    target_index: u32,
    kill_others: bool,
) -> Result<Vec<WindowRemovalPlan>, RmuxError> {
    window_pane_ids(session, session_name, target_index)?;

    let window_indices = if kill_others {
        session
            .windows()
            .keys()
            .copied()
            .filter(|window_index| *window_index != target_index)
            .collect::<Vec<_>>()
    } else {
        vec![target_index]
    };

    let mut seen_slots = HashSet::new();
    let mut removal_plan = Vec::new();
    for window_index in window_indices {
        let slots = expand_window_removal_slots(
            state,
            state.window_link_slots_for(session_name, window_index),
        );
        for slot in slots {
            if seen_slots.insert(slot.clone()) {
                removal_plan.push(build_window_slot_removal_plan(state, slot)?);
            }
        }
    }
    ensure_window_removal_leaves_survivors(state, &removal_plan)?;
    Ok(removal_plan)
}

fn expand_window_removal_slots(
    state: &HandlerState,
    root_slots: Vec<WindowLinkSlot>,
) -> Vec<WindowLinkSlot> {
    let mut seen = HashSet::new();
    let mut expanded = Vec::new();
    let mut pending = root_slots;

    while let Some(slot) = pending.pop() {
        if !seen.insert(slot.clone()) {
            continue;
        }

        for member in state.sessions.session_group_members(&slot.session_name) {
            pending.push(WindowLinkSlot {
                session_name: member,
                window_index: slot.window_index,
            });
        }
        for linked_slot in state.window_link_slots_for(&slot.session_name, slot.window_index) {
            pending.push(linked_slot);
        }
        expanded.push(slot);
    }

    expanded
}

fn build_window_slot_removal_plan(
    state: &HandlerState,
    slot: WindowLinkSlot,
) -> Result<WindowRemovalPlan, RmuxError> {
    let session = state
        .sessions
        .session(&slot.session_name)
        .ok_or_else(|| session_not_found(&slot.session_name))?;
    Ok(WindowRemovalPlan {
        runtime_session_name: state
            .runtime_session_name_for_window(&slot.session_name, slot.window_index),
        pane_ids: window_pane_ids(session, &slot.session_name, slot.window_index)?,
        session_name: slot.session_name,
        window_index: slot.window_index,
    })
}

fn ensure_window_removal_leaves_survivors(
    state: &HandlerState,
    removal_plan: &[WindowRemovalPlan],
) -> Result<(), RmuxError> {
    let mut removals_by_session = HashMap::<SessionName, usize>::new();
    for planned_window in removal_plan {
        *removals_by_session
            .entry(planned_window.session_name.clone())
            .or_default() += 1;
    }

    for (session_name, removed_count) in removals_by_session {
        let session = state
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        if session.windows().len() <= removed_count {
            return Err(RmuxError::Server(format!(
                "cannot kill the only window in session {session_name}"
            )));
        }
    }

    Ok(())
}

fn ensure_window_removal_terminals_exist(
    state: &HandlerState,
    removal_plan: &[WindowRemovalPlan],
) -> Result<(), RmuxError> {
    let mut panes_by_runtime = HashMap::<SessionName, Vec<PaneId>>::new();
    for planned_window in removal_plan {
        panes_by_runtime
            .entry(planned_window.runtime_session_name.clone())
            .or_default()
            .extend(planned_window.pane_ids.iter().copied());
    }

    for (runtime_session_name, pane_ids) in panes_by_runtime {
        state
            .terminals
            .ensure_panes_exist(&runtime_session_name, &pane_ids)?;
    }

    Ok(())
}
