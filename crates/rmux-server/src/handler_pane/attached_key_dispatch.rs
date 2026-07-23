use std::time::{Duration, Instant};

use rmux_core::key_code_lookup_bits;
use rmux_proto::{ErrorResponse, OptionName, PaneTarget, Response, RmuxError, Target};
use tracing::warn;

use super::super::copy_mode_support::key_binding::direct_copy_mode_command;
use super::super::RequestHandler;
use super::{attached_status_message_for_error, display_time, AttachedKeyDispatch};
use crate::key_table::{
    default_key_table_name, lookup_attached_key_table_binding, lookup_key_table_binding,
    matches_prefix_key, session_option_key, session_option_u64, should_drop_unbound_prefix_key,
    step03_prefix_binding, Step03PrefixBinding, COPY_MODE_TABLE, COPY_MODE_VI_TABLE, PREFIX_TABLE,
};
use crate::pane_terminals::session_not_found;
use crate::renderer;

#[path = "attached_key_dispatch/commands.rs"]
mod commands;

#[cfg(test)]
#[path = "attached_key_dispatch/test_support.rs"]
mod test_support;

use commands::{execute_attached_binding_commands, AttachedBindingCommandContext};

#[derive(Clone, Copy)]
struct AttachedKeyTableCommitContext<'a> {
    identity: super::super::attach_support::ActiveAttachIdentity,
    session_name: &'a rmux_proto::SessionName,
    expected_generation: u64,
}

impl RequestHandler {
    #[async_recursion::async_recursion]
    pub(super) async fn dispatch_attached_key(
        &self,
        attach_pid: u32,
        requester_pid: u32,
        target: &PaneTarget,
        key: rmux_core::KeyCode,
    ) -> Result<(), RmuxError> {
        let _ = Box::pin(self.dispatch_attached_key_inner(
            target,
            AttachedKeyDispatch {
                attach_pid,
                live_identity: None,
                live_session_id: None,
                requester_pid,
                current_target: Some(Target::Pane(target.clone())),
                mouse_target: None,
                mouse_event: None,
                key,
                attached_live_input: false,
            },
        ))
        .await?;
        Ok(())
    }

    #[async_recursion::async_recursion]
    pub(super) async fn dispatch_attached_key_inner(
        &self,
        target: &PaneTarget,
        dispatch: AttachedKeyDispatch,
    ) -> Result<bool, RmuxError> {
        let AttachedKeyDispatch {
            attach_pid,
            live_identity,
            live_session_id,
            requester_pid,
            current_target,
            mouse_target,
            mouse_event,
            key,
            attached_live_input,
        } = dispatch;

        let exited_clock_mode = match (live_identity, live_session_id) {
            (Some(identity), Some(session_id)) => {
                self.exit_clock_mode_for_attached_identity(target, identity, session_id)
                    .await?
            }
            (Some(_), None) => {
                return Err(RmuxError::Server(
                    "attached client session identity missing".to_owned(),
                ));
            }
            (None, _) => self.exit_clock_mode(target).await?,
        };
        if exited_clock_mode {
            return Ok(true);
        }

        let now = Instant::now();
        let snapshot = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    live_identity.is_none_or(|identity| {
                        identity.matches_active(active)
                            && live_session_id.is_some_and(|expected| {
                                active.session_id == expected
                                    && active.session_name == *target.session_name()
                            })
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            (
                active.identity(attach_pid),
                active.session_name.clone(),
                active.session_id,
                active.key_table_name.clone(),
                active.key_table_set_at,
                active.key_table_generation,
                active.repeat_deadline,
                active.repeat_active,
                active.last_key,
            )
        };

        let lookup_key = key_code_lookup_bits(key);
        let (
            key_table_identity,
            session_name,
            session_id,
            current_table_name,
            key_table_set_at,
            key_table_generation,
            repeat_deadline,
            repeat_active,
            last_key,
        ) = snapshot;
        let (
            default_table,
            prefix_key,
            prefix2_key,
            prefix_timeout_ms,
            repeat_time_ms,
            initial_repeat_time_ms,
            binding,
            should_enter_prefix,
            should_clear_before_dispatch,
            from_prefix_table,
        ) = {
            let state = self.state.lock().await;
            if live_session_id.is_some()
                && state
                    .sessions
                    .session(target.session_name())
                    .is_none_or(|session| session.id() != session_id)
            {
                return Ok(true);
            }
            let default_table = default_key_table_name(&state, target);
            let prefix_key = session_option_key(&state, &session_name, OptionName::Prefix);
            let prefix2_key = session_option_key(&state, &session_name, OptionName::Prefix2);
            let prefix_timeout_ms =
                session_option_u64(&state, &session_name, OptionName::PrefixTimeout);
            let repeat_time_ms = session_option_u64(&state, &session_name, OptionName::RepeatTime);
            let initial_repeat_time_ms =
                session_option_u64(&state, &session_name, OptionName::InitialRepeatTime);

            let mut table_name = current_table_name
                .clone()
                .unwrap_or_else(|| default_table.clone());
            let mut should_clear = false;

            if repeat_deadline.is_some_and(|deadline| now > deadline) {
                table_name = default_table.clone();
                should_clear = true;
            }
            if current_table_name.as_deref() == Some(PREFIX_TABLE)
                && prefix_timeout_ms != 0
                && !repeat_active
                && key_table_set_at.is_some_and(|set_at| {
                    now.duration_since(set_at).as_millis() > u128::from(prefix_timeout_ms)
                })
            {
                table_name = default_table.clone();
                should_clear = true;
            }

            let prefix_match = matches_prefix_key(lookup_key, prefix_key, prefix2_key);
            // tmux gives prefix/prefix2 precedence over every current table.
            // Keep an already-active prefix table so its send-prefix binding runs.
            if table_name != PREFIX_TABLE && prefix_match {
                (
                    default_table,
                    prefix_key,
                    prefix2_key,
                    prefix_timeout_ms,
                    repeat_time_ms,
                    initial_repeat_time_ms,
                    None,
                    true,
                    should_clear,
                    false,
                )
            } else {
                let from_prefix_table = table_name == PREFIX_TABLE;
                let lookup_binding = if attached_live_input {
                    lookup_attached_key_table_binding
                } else {
                    lookup_key_table_binding
                };
                let mut binding = lookup_binding(&state, &table_name, lookup_key);
                if repeat_active
                    && table_name != default_table
                    && binding.as_ref().is_some_and(|binding| !binding.repeat())
                {
                    table_name = default_table.clone();
                    binding = lookup_binding(&state, &table_name, lookup_key);
                    should_clear = true;
                }

                (
                    default_table,
                    prefix_key,
                    prefix2_key,
                    prefix_timeout_ms,
                    repeat_time_ms,
                    initial_repeat_time_ms,
                    binding,
                    false,
                    should_clear,
                    from_prefix_table,
                )
            }
        };

        #[cfg(test)]
        self.pause_attached_key_dispatch_after_lookup(attach_pid)
            .await;

        let key_table_commit = AttachedKeyTableCommitContext {
            identity: key_table_identity,
            session_name: &session_name,
            expected_generation: key_table_generation,
        };
        let _ = (prefix_key, prefix2_key);

        if should_enter_prefix {
            let Some(commit) = self
                .set_attached_key_table_for_dispatch(
                    key_table_commit,
                    Some(PREFIX_TABLE.to_owned()),
                    Some(now),
                )
                .await?
            else {
                return Ok(true);
            };
            let timer_identity = {
                let mut active_attach = self.active_attach.lock().await;
                let active = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .filter(|active| {
                        key_table_identity.matches_active_session(active, &session_name, session_id)
                    })
                    .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
                if active.key_table_generation != commit.key_table_generation {
                    return Ok(true);
                }
                active.repeat_active = false;
                active.repeat_deadline = None;
                active.last_key = None;
                (active.identity(attach_pid), active.key_table_generation)
            };
            if prefix_timeout_ms != 0 {
                self.schedule_attached_prefix_timeout_for_identity(
                    timer_identity.0,
                    now,
                    timer_identity.1,
                    prefix_timeout_ms,
                );
            }
            return Ok(true);
        }

        let Some(binding) = binding else {
            if current_table_name
                .as_deref()
                .is_some_and(|table_name| should_drop_unbound_prefix_key(table_name, lookup_key))
            {
                let commit = self
                    .set_attached_key_table_for_dispatch(key_table_commit, None, None)
                    .await?;
                let Some(commit) = commit else {
                    return Ok(true);
                };
                let mut active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .filter(|active| {
                        key_table_identity.matches_active_session(active, &session_name, session_id)
                    })
                    .filter(|active| active.key_table_generation == commit.key_table_generation)
                {
                    active.repeat_active = false;
                    active.repeat_deadline = None;
                    active.last_key = None;
                }
                return Ok(true);
            }
            if should_clear_before_dispatch
                || current_table_name
                    .as_deref()
                    .is_some_and(|table_name| table_name != default_table.as_str())
            {
                let commit = self
                    .set_attached_key_table_for_dispatch(key_table_commit, None, None)
                    .await?;
                let Some(commit) = commit else {
                    return Ok(true);
                };
                let mut active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .filter(|active| {
                        key_table_identity.matches_active_session(active, &session_name, session_id)
                    })
                    .filter(|active| active.key_table_generation == commit.key_table_generation)
                {
                    active.repeat_active = false;
                    active.repeat_deadline = None;
                    active.last_key = None;
                }
            }
            if matches!(default_table.as_str(), COPY_MODE_TABLE | COPY_MODE_VI_TABLE) {
                return Ok(true);
            }
            return Ok(false);
        };

        let first_repeat = !repeat_active || last_key != Some(binding.key());
        let repeat_window_ms = if binding.repeat() {
            if first_repeat && initial_repeat_time_ms != 0 {
                initial_repeat_time_ms
            } else {
                repeat_time_ms
            }
        } else {
            0
        };
        let repeat_deadline = binding
            .repeat()
            .then_some(now + Duration::from_millis(repeat_window_ms.max(1)));
        let should_return_to_default = current_table_name
            .as_deref()
            .is_some_and(|table_name| table_name != default_table)
            && !binding.repeat();

        // Binding lookup happened before awaits. Treat its table generation as a
        // CAS token so an older dispatch cannot install repeat state on a newer
        // table selection.
        let expected_repeat_generation = if should_return_to_default || should_clear_before_dispatch
        {
            self.set_attached_key_table_for_dispatch(key_table_commit, None, None)
                .await?
                .map(|commit| commit.key_table_generation)
        } else {
            Some(key_table_generation)
        };
        let timer_identity = if let Some(expected_repeat_generation) = expected_repeat_generation {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    key_table_identity.matches_active_session(active, &session_name, session_id)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if active.key_table_generation != expected_repeat_generation {
                None
            } else {
                if binding.repeat() {
                    active.repeat_active = true;
                    active.repeat_deadline = repeat_deadline;
                    active.last_key = Some(binding.key());
                } else {
                    active.repeat_active = false;
                    active.repeat_deadline = None;
                    active.last_key = Some(binding.key());
                }
                Some((active.identity(attach_pid), active.key_table_generation))
            }
        } else {
            None
        };
        if let (Some(timer_identity), Some(repeat_deadline)) = (timer_identity, repeat_deadline) {
            self.schedule_attached_repeat_timeout_for_identity(
                timer_identity.0,
                repeat_deadline,
                timer_identity.1,
            );
        }

        if from_prefix_table {
            if let Some(action) = step03_prefix_binding(lookup_key) {
                if let Err(error) = self.dispatch_step03_prefix_action(action, target).await {
                    if attached_live_input {
                        self.report_attached_command_error(&session_name, attach_pid, &error)
                            .await;
                        return Ok(true);
                    }
                    return Err(error);
                }
                return Ok(true);
            }
        }

        if let Some(command) = direct_copy_mode_command(binding.commands()) {
            Box::pin(self.execute_direct_copy_mode_binding(
                requester_pid,
                live_identity,
                target.clone(),
                &command,
                mouse_event,
            ))
            .await?;
            return Ok(true);
        }

        let dispatch_target = current_target.unwrap_or_else(|| Target::Pane(target.clone()));
        Box::pin(execute_attached_binding_commands(
            self,
            AttachedBindingCommandContext {
                attach_pid,
                live_identity,
                requester_pid,
                session_name: session_name.clone(),
                session_id,
                attached_live_input,
                dispatch_target,
                mouse_target,
                mouse_event,
                commands: binding.commands().clone(),
            },
        ))
        .await?;
        Ok(true)
    }

    async fn set_attached_key_table_for_dispatch(
        &self,
        context: AttachedKeyTableCommitContext<'_>,
        key_table_name: Option<String>,
        key_table_set_at: Option<Instant>,
    ) -> Result<Option<super::super::attach_support::AttachedKeyTableCommit>, RmuxError> {
        self.set_attached_key_table_for_client_session_identity_if_generation(
            context.identity,
            context.session_name,
            context.identity.session_id(),
            context.expected_generation,
            key_table_name,
            key_table_set_at,
        )
        .await
    }

    async fn report_attached_command_error(
        &self,
        session_name: &rmux_proto::SessionName,
        attach_pid: u32,
        error: &RmuxError,
    ) {
        warn!(
            attach_pid,
            session = %session_name,
            "attached input command failed: {error}"
        );

        let message = attached_status_message_for_error(error);
        let (overlay_frame, clear_frame, duration) = {
            let mut state = self.state.lock().await;
            state.add_message(message.clone());
            let Some(session) = state.sessions.session(session_name) else {
                return;
            };
            let mut overlay_frame =
                renderer::render_display_panes_clear(session, &state.options, &state);
            overlay_frame.extend_from_slice(
                renderer::render_status_message(session, &state.options, &message).as_slice(),
            );
            let clear_frame = renderer::render_display_panes_clear(session, &state.options, &state);
            let duration = display_time(&state.options, session_name);
            (overlay_frame, clear_frame, duration)
        };

        let _ = self
            .send_attached_overlay(session_name, overlay_frame, clear_frame, duration)
            .await;
    }

    async fn dispatch_step03_prefix_action(
        &self,
        action: Step03PrefixBinding,
        target: &PaneTarget,
    ) -> Result<(), RmuxError> {
        match action {
            Step03PrefixBinding::SelectPaneNext | Step03PrefixBinding::SelectPanePrevious => {
                let target = {
                    let state = self.state.lock().await;
                    let session = state
                        .sessions
                        .session(target.session_name())
                        .ok_or_else(|| session_not_found(target.session_name()))?;
                    let window = session.window_at(target.window_index()).ok_or_else(|| {
                        RmuxError::invalid_target(
                            target.to_string(),
                            "window index does not exist in session",
                        )
                    })?;
                    let panes = window.panes();
                    let active = window.active_pane_index();
                    let Some(position) = panes.iter().position(|pane| pane.index() == active)
                    else {
                        return Err(RmuxError::invalid_target(
                            target.to_string(),
                            "active pane index does not exist in window",
                        ));
                    };
                    let selected_position = match action {
                        Step03PrefixBinding::SelectPaneNext => (position + 1) % panes.len(),
                        Step03PrefixBinding::SelectPanePrevious => {
                            (position + panes.len() - 1) % panes.len()
                        }
                        _ => unreachable!("action filtered by outer match"),
                    };
                    PaneTarget::with_window(
                        target.session_name().clone(),
                        target.window_index(),
                        panes[selected_position].index(),
                    )
                };
                let response = self
                    .handle_select_pane(rmux_proto::SelectPaneRequest {
                        target,
                        title: None,
                        style: None,
                        input_disabled: None,
                        preserve_zoom: false,
                    })
                    .await;
                match response {
                    Response::SelectPane(_) => Ok(()),
                    Response::Error(ErrorResponse { error }) => Err(error),
                    _ => Err(RmuxError::Server(
                        "select-pane prefix binding returned unexpected response".to_owned(),
                    )),
                }
            }
            Step03PrefixBinding::NextWindow => {
                let response = self
                    .handle_next_window(rmux_proto::NextWindowRequest {
                        target: target.session_name().clone(),
                        alerts_only: false,
                    })
                    .await;
                match response {
                    Response::NextWindow(_) => Ok(()),
                    Response::Error(ErrorResponse { error }) => Err(error),
                    _ => Err(RmuxError::Server(
                        "next-window prefix binding returned unexpected response".to_owned(),
                    )),
                }
            }
            Step03PrefixBinding::PreviousWindow => {
                let response = self
                    .handle_previous_window(rmux_proto::PreviousWindowRequest {
                        target: target.session_name().clone(),
                        alerts_only: false,
                    })
                    .await;
                match response {
                    Response::PreviousWindow(_) => Ok(()),
                    Response::Error(ErrorResponse { error }) => Err(error),
                    _ => Err(RmuxError::Server(
                        "previous-window prefix binding returned unexpected response".to_owned(),
                    )),
                }
            }
        }
    }
}
